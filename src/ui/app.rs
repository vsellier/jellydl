//! App state, input handling and the main loop.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use futures::StreamExt;
use ratatui::widgets::TableState;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use tokio::sync::mpsc;

use super::input::TextInput;
use super::view::{ListView, LoadKind, PAGE_SIZE, PageAction, Source};
use crate::api::{BaseItem, Jellyfin, NewSet, Person, QueryResult, Sort, SortField, date};
use crate::download::{Command, Event as MgrEvent, Job, JobState, ManagerHandle, NewJob};
use crate::paths;

/// Rows moved by PgUp / PgDn.
const PAGE_STEP: usize = 20;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
const MESSAGE_TTL: Duration = Duration::from_secs(5);
/// The cast is loaded when the selection stays on an item this long.
const CAST_DELAY: Duration = Duration::from_millis(250);

/// Poster height asked from the server, in pixels. The widget scales it down.
const POSTER_HEIGHT: u32 = 600;
/// Max number of decoded posters kept in memory.
const POSTER_CACHE: usize = 40;

pub enum Poster {
    Loading,
    Ready(Box<StatefulProtocol>),
    /// No image on the server, or it could not be loaded or decoded.
    Missing,
}

/// Posters by item id. The oldest ones are dropped after `POSTER_CACHE` entries.
#[derive(Default)]
pub struct PosterCache {
    map: HashMap<String, Poster>,
    order: VecDeque<String>,
}

impl PosterCache {
    fn contains(&self, id: &str) -> bool {
        self.map.contains_key(id)
    }

    pub(crate) fn insert(&mut self, id: String, p: Poster) {
        if self.map.insert(id.clone(), p).is_none() {
            self.order.push_back(id);
        }
        while self.order.len() > POSTER_CACHE {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Poster> {
        self.map.get_mut(id)
    }
}

/// Download and decode the poster of `it`. Episodes and seasons without an image
/// use the series poster.
async fn load_poster(api: &Jellyfin, it: &BaseItem, picker: Picker) -> Poster {
    let mut ids = vec![it.id.clone()];
    if matches!(it.item_type.as_str(), "Episode" | "Season")
        && let Some(s) = &it.series_id
    {
        ids.push(s.clone());
    }
    for id in ids {
        match api.primary_image(&id, POSTER_HEIGHT).await {
            Ok(Some(bytes)) => {
                // Decoding is CPU work: keep it off the async threads.
                let decoded = tokio::task::spawn_blocking(move || {
                    image::load_from_memory(&bytes).map(|img| picker.new_resize_protocol(img))
                })
                .await;
                return match decoded {
                    Ok(Ok(p)) => Poster::Ready(Box::new(p)),
                    Ok(Err(e)) => {
                        tracing::warn!("cannot decode the poster of {id}: {e}");
                        Poster::Missing
                    }
                    Err(e) => {
                        tracing::warn!("poster task failed: {e}");
                        Poster::Missing
                    }
                };
            }
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!("cannot load the poster of {id}: {e:#}");
                return Poster::Missing;
            }
        }
    }
    Poster::Missing
}

/// Cast of an item, as shown in the details pane.
pub enum Cast {
    Loading,
    Loaded(Vec<Person>),
    Failed,
}

pub enum AppEvent {
    Page {
        view: u64,
        req: u64,
        result: Result<QueryResult, String>,
    },
    Collected(Result<Vec<BaseItem>, String>),
    People {
        item_id: String,
        result: Result<Vec<Person>, String>,
    },
    Poster {
        item_id: String,
        poster: Poster,
    },
    /// Which items are new, for the "new" tag in the lists.
    NewSet(Result<NewSet, String>),
    Mgr(MgrEvent),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Library,
    Search,
    Downloads,
}

pub enum Popup {
    Help,
    ConfirmQuit,
    /// Sort menu of the current library list. `cursor` is an index in `SortField::ALL`.
    Sort {
        cursor: usize,
        descending: bool,
    },
    /// Set the last check to now: nothing is new after that.
    ConfirmMarkSeen,
    ConfirmEnqueue {
        jobs: Vec<NewJob>,
        size: u64,
        unknown_size: usize,
        skipped: usize,
    },
}

pub struct Message {
    pub text: String,
    pub error: bool,
    at: Instant,
}

pub struct App {
    pub api: Jellyfin,
    pub tx: mpsc::UnboundedSender<AppEvent>,
    pub mgr: ManagerHandle,
    pub download_dir: PathBuf,
    pub proxy_label: Option<String>,

    pub tab: Tab,
    pub lib_stack: Vec<ListView>,
    pub search: ListView,
    pub search_input: TextInput,
    pub search_editing: bool,
    search_due: Option<Instant>,

    /// Cast by item id. Kept for the whole session.
    pub cast: HashMap<String, Cast>,
    /// Selected item and since when, to load its cast and poster.
    cast_sel: Option<(String, Instant)>,
    /// Image support of the terminal. `None` when posters are off.
    picker: Option<Picker>,
    pub posters: PosterCache,

    pub jobs: Vec<Job>,
    pub dl_state: TableState,
    pub max_parallel: usize,
    pub speed_limit: Option<u64>,
    /// Sort of the last library list sorted. New library lists start with it.
    pub sort: Sort,
    /// Where the sort is saved between runs. `None`: not saved (tests).
    state_file: Option<PathBuf>,
    /// Items added after this time (Unix seconds) are new. Changed only with `N`.
    pub last_check: i64,
    /// Lists show only the new items (`n`). The first screen is never filtered.
    pub new_only: bool,
    /// Which items are new, for the tag in the lists.
    pub new_set: NewSet,

    pub popup: Option<Popup>,
    pub message: Option<Message>,
    pub ticks: u64,
    pub quit: bool,
    next_view_id: u64,
}

impl App {
    pub fn new(
        api: Jellyfin,
        tx: mpsc::UnboundedSender<AppEvent>,
        mgr: ManagerHandle,
        download_dir: PathBuf,
        proxy_label: Option<String>,
        max_parallel: usize,
        speed_limit: Option<u64>,
    ) -> Self {
        let mut app = Self {
            api,
            tx,
            mgr,
            download_dir,
            proxy_label,
            tab: Tab::Library,
            lib_stack: Vec::new(),
            search: ListView::new(
                0,
                "Search",
                Source::Search {
                    term: String::new(),
                },
            ),
            search_input: TextInput::default(),
            search_editing: false,
            search_due: None,
            cast: HashMap::new(),
            cast_sel: None,
            picker: None,
            posters: PosterCache::default(),
            jobs: Vec::new(),
            dl_state: TableState::default(),
            max_parallel,
            speed_limit,
            sort: Sort::default(),
            state_file: None,
            last_check: date::now(),
            new_only: false,
            new_set: NewSet::empty(0),
            popup: None,
            message: None,
            ticks: 0,
            quit: false,
            next_view_id: 1,
        };
        let root = app.new_view("Libraries", Source::Libraries);
        app.lib_stack.push(root);
        app.new_set = NewSet::empty(app.last_check);
        app.load(app.lib_stack.len() - 1, 0, LoadKind::Replace, None);
        app
    }

    /// Use the sort and the last check saved in `path`, and save them there when
    /// they change. With no saved last check, it starts now: nothing is new yet.
    pub fn with_state_file(mut self, path: PathBuf) -> Self {
        let state = super::state::load(&path);
        self.sort = state.sort;
        self.state_file = Some(path);
        match state.last_check {
            Some(t) => self.last_check = t,
            None => {
                self.last_check = date::now();
                self.save_state();
            }
        }
        self.new_set = NewSet::empty(self.last_check);
        self.refresh_new_set();
        self
    }

    fn save_state(&self) {
        let Some(path) = &self.state_file else {
            return;
        };
        let s = super::state::UiState {
            sort: self.sort,
            last_check: Some(self.last_check),
        };
        if let Err(e) = super::state::save(path, &s) {
            tracing::warn!("cannot save the UI state: {e:#}");
        }
    }

    /// Find again which items are new, for the tag in the lists.
    fn refresh_new_set(&self) {
        let (api, tx, since) = (self.api.clone(), self.tx.clone(), self.last_check);
        tokio::spawn(async move {
            let result = api
                .new_set(None, since, true)
                .await
                .map(|(_, set)| set)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::NewSet(result));
        });
    }

    /// The filter date the current list should have.
    fn wanted_filter(&self, v: &ListView) -> Option<i64> {
        let filterable = !matches!(v.source, Source::Libraries);
        (self.new_only && filterable).then_some(self.last_check)
    }

    /// Reload the current list when its filter differs from the wanted one: after
    /// `n`, `N`, going back, or a tab change.
    fn sync_filter(&mut self) {
        let wanted = match self.current_view() {
            Some(v) => self.wanted_filter(v),
            None => return,
        };
        let (api, tx) = (self.api.clone(), self.tx.clone());
        let Some(v) = self.current_view_mut() else {
            return;
        };
        if v.new_since == wanted {
            return;
        }
        v.new_since = wanted;
        v.clear_marks();
        // An empty search box: nothing to load yet.
        if matches!(&v.source, Source::Search { term } if term.is_empty()) {
            return;
        }
        Self::spawn_load(&api, &tx, v, 0, LoadKind::Replace, None);
    }

    /// `n`: show only the new items, or all of them again.
    fn toggle_new_only(&mut self) {
        self.new_only = !self.new_only;
        if self.new_only {
            let since = date::format_utc(self.last_check);
            let on_libraries = self.tab == Tab::Library && self.lib_stack.len() == 1;
            if on_libraries {
                self.info(format!(
                    "New items only (since {since}): open a library to see them"
                ));
            } else {
                self.info(format!("New items only (since {since})"));
            }
            self.refresh_new_set();
        } else {
            self.info("All items");
        }
        self.sync_filter();
    }

    /// `N` confirmed: the last check is now. Nothing is new any more.
    fn mark_seen(&mut self) {
        self.last_check = date::now();
        self.new_set = NewSet::empty(self.last_check);
        self.save_state();
        self.info(format!(
            "Last check set to {}",
            date::format_utc(self.last_check)
        ));
        self.sync_filter();
    }

    fn new_view(&mut self, title: impl Into<String>, source: Source) -> ListView {
        self.next_view_id += 1;
        ListView::new(self.next_view_id, title, source)
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: false,
            at: Instant::now(),
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: true,
            at: Instant::now(),
        });
    }

    pub fn current_view(&self) -> Option<&ListView> {
        match self.tab {
            Tab::Library => self.lib_stack.last(),
            Tab::Search => Some(&self.search),
            Tab::Downloads => None,
        }
    }

    fn current_view_mut(&mut self) -> Option<&mut ListView> {
        match self.tab {
            Tab::Library => self.lib_stack.last_mut(),
            Tab::Search => Some(&mut self.search),
            Tab::Downloads => None,
        }
    }

    fn view_by_id(&mut self, id: u64) -> Option<&mut ListView> {
        if self.search.id == id {
            return Some(&mut self.search);
        }
        self.lib_stack.iter_mut().find(|v| v.id == id)
    }

    pub fn active_count(&self) -> (usize, usize) {
        let running = self
            .jobs
            .iter()
            .filter(|j| j.state == JobState::Running)
            .count();
        let queued = self
            .jobs
            .iter()
            .filter(|j| j.state == JobState::Queued)
            .count();
        (running, queued)
    }

    pub fn total_speed(&self) -> f64 {
        self.jobs
            .iter()
            .filter(|j| j.state == JobState::Running)
            .map(|j| j.speed)
            .sum()
    }

    // ---------------------------------------------------------------- loading

    /// Load a page of the view at `idx` in the library stack.
    fn load(&mut self, idx: usize, start: usize, kind: LoadKind, select: Option<usize>) {
        let v = &mut self.lib_stack[idx];
        Self::spawn_load(&self.api, &self.tx, v, start, kind, select);
    }

    fn spawn_load(
        api: &Jellyfin,
        tx: &mpsc::UnboundedSender<AppEvent>,
        v: &mut ListView,
        start: usize,
        kind: LoadKind,
        select: Option<usize>,
    ) {
        let req = v.begin_load(start, kind, select);
        let (api, tx, source, view, sort, since) = (
            api.clone(),
            tx.clone(),
            v.source.clone(),
            v.id,
            v.sort,
            v.new_since,
        );
        tokio::spawn(async move {
            let result = if let Some(since) = since {
                super::new_items::load(&api, source, sort, since).await
            } else {
                match source {
                    Source::Libraries => api.views().await.map(|items| QueryResult {
                        total_record_count: items.len(),
                        items,
                    }),
                    Source::Children {
                        parent_id,
                        collection_type,
                    } => {
                        api.children(
                            &parent_id,
                            collection_type.as_deref(),
                            sort,
                            start,
                            PAGE_SIZE,
                        )
                        .await
                    }
                    Source::Seasons { series_id } => api.seasons(&series_id).await,
                    Source::Episodes {
                        series_id,
                        season_id,
                    } => api.episodes(&series_id, &season_id).await,
                    Source::Search { term } => api.search(&term).await,
                }
            };
            let _ = tx.send(AppEvent::Page {
                view,
                req,
                result: result.map_err(|e| format!("{e:#}")),
            });
        });
    }

    /// Load the next or previous page when the selection is near an edge of the window.
    fn maybe_load_more(&mut self) {
        if self.tab != Tab::Library {
            return;
        }
        let idx = self.lib_stack.len() - 1;
        let v = &self.lib_stack[idx];
        if v.wants_next_page() {
            let start = v.offset + v.items.len();
            self.load(idx, start, LoadKind::Append, None);
        } else if v.wants_prev_page() {
            let start = v.offset.saturating_sub(PAGE_SIZE);
            self.load(idx, start, LoadKind::Prepend, None);
        }
    }

    /// Show a page of the current list, and load it when needed.
    fn goto_page(&mut self, page: impl FnOnce(&ListView) -> usize) {
        let (api, tx) = (self.api.clone(), self.tx.clone());
        let Some(v) = self.current_view_mut() else {
            return;
        };
        if v.loading() && v.paged() {
            return;
        }
        let target = page(v);
        if let PageAction::Load { start, kind } = v.goto_page(target) {
            Self::spawn_load(&api, &tx, v, start, kind, Some(start));
        }
    }

    /// Reload the current list. Paged lists reload the page of the selection.
    fn reload(&mut self) {
        self.refresh_new_set();
        if self.tab != Tab::Library {
            self.run_search();
            return;
        }
        let idx = self.lib_stack.len() - 1;
        let v = &mut self.lib_stack[idx];
        v.clear_marks();
        let (start, select) = if v.paged() {
            let abs = v.offset + v.state.selected().unwrap_or(0);
            (v.current_page() * PAGE_SIZE, Some(abs))
        } else {
            (0, v.state.selected())
        };
        self.load(idx, start, LoadKind::Replace, select);
    }

    /// The current list, when it can be sorted.
    fn sortable_view(&mut self) -> Option<&mut ListView> {
        let ok =
            self.tab == Tab::Library && self.lib_stack.last().is_some_and(|v| v.source.is_paged());
        if !ok {
            self.info("Sorting works in library and folder lists");
            return None;
        }
        self.lib_stack.last_mut()
    }

    fn open_sort_menu(&mut self) {
        let Some(v) = self.sortable_view() else {
            return;
        };
        let cursor = SortField::ALL
            .iter()
            .position(|f| *f == v.sort.field)
            .unwrap_or(0);
        let descending = v.sort.descending;
        self.popup = Some(Popup::Sort { cursor, descending });
    }

    /// Sort the current library list and load it again from the first page.
    fn apply_sort(&mut self, sort: Sort) {
        let Some(v) = self.sortable_view() else {
            return;
        };
        if v.sort == sort {
            return;
        }
        v.sort = sort;
        self.sort = sort;
        self.save_state();
        self.info(format!("Sorted by {}", sort.label()));
        self.load(self.lib_stack.len() - 1, 0, LoadKind::Replace, None);
    }

    fn run_search(&mut self) {
        self.search_due = None;
        let term = self.search_input.text().trim().to_string();
        if term.is_empty() {
            return;
        }
        self.next_view_id += 1;
        // A new id drops replies from older searches.
        self.search.id = self.next_view_id;
        self.search.source = Source::Search { term: term.clone() };
        self.search.title = format!("Search: {term}");
        Self::spawn_load(
            &self.api,
            &self.tx,
            &mut self.search,
            0,
            LoadKind::Replace,
            None,
        );
    }

    // ----------------------------------------------------------------- events

    pub fn on_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Page { view, req, result } => {
                let Some(v) = self.view_by_id(view) else {
                    return;
                };
                match result {
                    Ok(r) => {
                        v.apply_page(req, r.items, r.total_record_count);
                    }
                    Err(e) => {
                        if v.fail(req, e.clone()) {
                            self.error(e);
                        }
                    }
                }
            }
            AppEvent::Collected(Ok(items)) => self.prepare_enqueue(items),
            AppEvent::Collected(Err(e)) => self.error(format!("cannot list the videos: {e}")),
            AppEvent::Poster { item_id, poster } => self.posters.insert(item_id, poster),
            AppEvent::People { item_id, result } => {
                let cast = match result {
                    Ok(p) => Cast::Loaded(p),
                    Err(e) => {
                        tracing::warn!("cannot load the cast of {item_id}: {e}");
                        Cast::Failed
                    }
                };
                self.cast.insert(item_id, cast);
            }
            AppEvent::Mgr(MgrEvent::Updated(job)) => {
                match self.jobs.binary_search_by_key(&job.id, |j| j.id) {
                    Ok(i) => {
                        let was = self.jobs[i].state;
                        if was != job.state {
                            match job.state {
                                JobState::Done => self.info(format!("Done: {}", job.title)),
                                JobState::Failed => self.error(format!("Failed: {}", job.title)),
                                _ => {}
                            }
                        }
                        self.jobs[i] = job;
                    }
                    Err(i) => self.jobs.insert(i, job),
                }
                if self.dl_state.selected().is_none() && !self.jobs.is_empty() {
                    self.dl_state.select(Some(0));
                }
            }
            AppEvent::Mgr(MgrEvent::Removed(id)) => {
                self.jobs.retain(|j| j.id != id);
                let n = self.jobs.len();
                match self.dl_state.selected() {
                    _ if n == 0 => self.dl_state.select(None),
                    Some(s) if s >= n => self.dl_state.select(Some(n - 1)),
                    _ => {}
                }
            }
            // A reply for an older last check is dropped.
            AppEvent::NewSet(Ok(set)) if set.since == self.last_check => self.new_set = set,
            AppEvent::NewSet(Ok(_)) => {}
            AppEvent::NewSet(Err(e)) => tracing::warn!("cannot find the new items: {e}"),
            AppEvent::Mgr(MgrEvent::MaxParallel(n)) => self.max_parallel = n,
        }
    }

    pub fn on_tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        self.maybe_load_details();
        if self
            .search_due
            .is_some_and(|t| t.elapsed() >= SEARCH_DEBOUNCE)
        {
            self.run_search();
        }
        if self
            .message
            .as_ref()
            .is_some_and(|m| m.at.elapsed() > MESSAGE_TTL)
        {
            self.message = None;
        }
    }

    /// Load the cast and poster of the selected item once the selection has stopped moving.
    fn maybe_load_details(&mut self) {
        let Some(it) = self.current_view().and_then(|v| v.selected()) else {
            self.cast_sel = None;
            return;
        };
        let id = it.id.clone();
        match &self.cast_sel {
            Some((sel, at)) if *sel == id => {
                if at.elapsed() < CAST_DELAY {
                    return;
                }
            }
            _ => {
                self.cast_sel = Some((id, Instant::now()));
                return;
            }
        }
        let it = it.clone();
        if it.has_people() && !self.cast.contains_key(&id) {
            self.cast.insert(id.clone(), Cast::Loading);
            let (api, tx, id) = (self.api.clone(), self.tx.clone(), id.clone());
            tokio::spawn(async move {
                let result = api.people(&id).await.map_err(|e| format!("{e:#}"));
                let _ = tx.send(AppEvent::People {
                    item_id: id,
                    result,
                });
            });
        }
        // Libraries get no poster: their images are banners, not posters.
        if let Some(picker) = &self.picker
            && !it.is_library()
            && !self.posters.contains(&id)
        {
            self.posters.insert(id.clone(), Poster::Loading);
            let (api, tx, picker) = (self.api.clone(), self.tx.clone(), picker.clone());
            tokio::spawn(async move {
                let poster = load_poster(&api, &it, picker).await;
                let _ = tx.send(AppEvent::Poster {
                    item_id: it.id,
                    poster,
                });
            });
        }
    }

    // --------------------------------------------------------------- download

    /// Start a download of the marked (or selected) items.
    fn download(&mut self) {
        let Some(v) = self.current_view_mut() else {
            return;
        };
        let targets = v.targets();
        v.clear_marks();
        if targets.is_empty() {
            return;
        }
        let (direct, folders): (Vec<_>, Vec<_>) =
            targets.into_iter().partition(|i| i.is_playable());
        if folders.is_empty() {
            self.prepare_enqueue(direct);
            return;
        }
        self.info(format!(
            "Listing the content of {} item(s)...",
            folders.len()
        ));
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            let mut all = direct;
            for f in folders {
                match api.descendants(&f.id).await {
                    Ok(items) => all.extend(items),
                    Err(e) => {
                        let _ = tx.send(AppEvent::Collected(Err(format!("{}: {e:#}", f.name))));
                        return;
                    }
                }
            }
            let _ = tx.send(AppEvent::Collected(Ok(all)));
        });
    }

    fn prepare_enqueue(&mut self, items: Vec<BaseItem>) {
        let busy: HashSet<&str> = self
            .jobs
            .iter()
            .filter(|j| {
                matches!(
                    j.state,
                    JobState::Queued | JobState::Running | JobState::Paused
                )
            })
            .map(|j| j.item_id.as_str())
            .collect();
        let mut seen = HashSet::new();
        let (mut jobs, mut skipped, mut size, mut unknown_size) = (Vec::new(), 0, 0u64, 0);
        for it in items.into_iter().filter(|i| i.is_playable()) {
            if busy.contains(it.id.as_str()) || !seen.insert(it.id.clone()) {
                skipped += 1;
                continue;
            }
            let target = paths::target_for(&it, &self.download_dir);
            let expected = it.size();
            if downloaded_path(&it, &self.download_dir).is_some() {
                skipped += 1;
                continue;
            }
            match expected {
                Some(s) => size += s,
                None => unknown_size += 1,
            }
            jobs.push(NewJob {
                url: self.api.download_url(&it.id),
                title: it.label(),
                item_id: it.id,
                target,
                expected_size: expected,
            });
        }
        match jobs.len() {
            0 if skipped > 0 => self.info("Nothing to do: already downloaded or in the queue"),
            0 => self.info("Nothing to download here"),
            1 if skipped == 0 => {
                self.info(format!("Queued: {}", jobs[0].title));
                self.mgr.send(Command::Enqueue(jobs));
            }
            _ => {
                self.popup = Some(Popup::ConfirmEnqueue {
                    jobs,
                    size,
                    unknown_size,
                    skipped,
                })
            }
        }
    }

    // ------------------------------------------------------------------ input

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.kind != KeyEventKind::Press {
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if let Some(p) = self.popup.take() {
            self.on_popup_key(p, k);
            return;
        }
        if self.tab == Tab::Search && self.search_editing && self.on_search_input(k) {
            return;
        }
        match k.code {
            KeyCode::Tab => self.switch_tab(1),
            KeyCode::BackTab => self.switch_tab(-1),
            KeyCode::Char('1') => self.tab = Tab::Library,
            KeyCode::Char('2') => self.tab = Tab::Search,
            KeyCode::Char('3') => self.tab = Tab::Downloads,
            KeyCode::Char('/') => {
                self.tab = Tab::Search;
                self.search_editing = true;
            }
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Char('q') => {
                let (r, q) = self.active_count();
                if r + q > 0 {
                    self.popup = Some(Popup::ConfirmQuit)
                } else {
                    self.quit = true
                }
            }
            _ if self.tab == Tab::Downloads => self.on_downloads_key(k),
            _ => self.on_list_key(k),
        }
        self.sync_filter();
    }

    fn switch_tab(&mut self, d: i32) {
        let tabs = [Tab::Library, Tab::Search, Tab::Downloads];
        let i = tabs.iter().position(|t| *t == self.tab).unwrap_or(0) as i32;
        self.tab = tabs[(i + d).rem_euclid(3) as usize];
    }

    fn on_popup_key(&mut self, p: Popup, k: KeyEvent) {
        let yes = matches!(
            k.code,
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter
        );
        match p {
            Popup::Help => {}
            Popup::Sort { cursor, descending } => self.on_sort_key(cursor, descending, k),
            Popup::ConfirmQuit => self.quit = yes,
            Popup::ConfirmMarkSeen => {
                if yes {
                    self.mark_seen();
                }
            }
            Popup::ConfirmEnqueue { jobs, .. } => {
                if yes {
                    self.info(format!("Queued {} download(s)", jobs.len()));
                    self.mgr.send(Command::Enqueue(jobs));
                }
            }
        }
    }

    fn on_sort_key(&mut self, cursor: usize, descending: bool, k: KeyEvent) {
        let n = SortField::ALL.len();
        let (cursor, descending) = match k.code {
            KeyCode::Up | KeyCode::Char('k') => ((cursor + n - 1) % n, descending),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('s') => {
                ((cursor + 1) % n, descending)
            }
            KeyCode::Left | KeyCode::Char('h') => (cursor, false),
            KeyCode::Right | KeyCode::Char('l') => (cursor, true),
            KeyCode::Char(' ') | KeyCode::Char('o') | KeyCode::Tab => (cursor, !descending),
            KeyCode::Enter => {
                let field = SortField::ALL[cursor];
                self.apply_sort(Sort { field, descending });
                return;
            }
            // Esc, q and other keys close the menu without a change.
            _ => return,
        };
        self.popup = Some(Popup::Sort { cursor, descending });
    }

    /// Keys while typing in the search box. Returns false to let the key go on.
    fn on_search_input(&mut self, k: KeyEvent) -> bool {
        if let Some(changed) = self.search_input.on_key(k) {
            if changed {
                self.search_due = Some(Instant::now());
            }
            return true;
        }
        match k.code {
            KeyCode::Enter => {
                self.search_editing = false;
                self.run_search();
            }
            KeyCode::Esc | KeyCode::Down => self.search_editing = false,
            // Other Ctrl keys do nothing while typing: Ctrl+Q must not ask to quit.
            KeyCode::Char(_) => {}
            _ => return false,
        }
        true
    }

    fn on_list_key(&mut self, k: KeyEvent) {
        let at_top = self
            .current_view()
            .and_then(|v| v.state.selected())
            .unwrap_or(0)
            == 0;
        match k.code {
            KeyCode::Up | KeyCode::Char('k') if at_top && self.tab == Tab::Search => {
                self.search_editing = true;
            }
            KeyCode::Up
            | KeyCode::Char('k')
            | KeyCode::Down
            | KeyCode::Char('j')
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::Char('g')
            | KeyCode::End
            | KeyCode::Char('G')
            | KeyCode::Char(' ')
            | KeyCode::Char('a') => {
                let Some(v) = self.current_view_mut() else {
                    return;
                };
                match k.code {
                    KeyCode::Up | KeyCode::Char('k') => v.move_by(-1),
                    KeyCode::Down | KeyCode::Char('j') => v.move_by(1),
                    KeyCode::PageUp => v.move_by(-(PAGE_STEP as isize)),
                    KeyCode::PageDown => v.move_by(PAGE_STEP as isize),
                    KeyCode::Home | KeyCode::Char('g') => v.select_first(),
                    KeyCode::End | KeyCode::Char('G') => v.select_last(),
                    KeyCode::Char(' ') => {
                        v.toggle_mark();
                        v.move_by(1);
                    }
                    _ => v.toggle_mark_all(),
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected(),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                if self.tab == Tab::Library && self.lib_stack.len() > 1 {
                    self.lib_stack.pop();
                } else if self.tab == Tab::Search {
                    self.search_editing = true;
                }
            }
            KeyCode::Char('d') => self.download(),
            KeyCode::Char('n') => self.toggle_new_only(),
            KeyCode::Char('N') => self.popup = Some(Popup::ConfirmMarkSeen),
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('s') => self.open_sort_menu(),
            KeyCode::Char('o') => {
                if let Some(v) = self.sortable_view() {
                    let sort = Sort {
                        descending: !v.sort.descending,
                        ..v.sort
                    };
                    self.apply_sort(sort);
                }
            }
            KeyCode::Char(']') => self.goto_page(|v| v.current_page() + 1),
            KeyCode::Char('[') => self.goto_page(|v| v.current_page().saturating_sub(1)),
            KeyCode::Char('{') => self.goto_page(|_| 0),
            KeyCode::Char('}') => self.goto_page(|v| v.page_count() - 1),
            _ => {}
        }
        self.maybe_load_more();
    }

    fn open_selected(&mut self) {
        let Some(item) = self.current_view().and_then(|v| v.selected()).cloned() else {
            return;
        };
        let (title, source) = match item.item_type.as_str() {
            "Series" => (
                item.name.clone(),
                Source::Seasons {
                    series_id: item.id.clone(),
                },
            ),
            "Season" => {
                let Some(series_id) = item.series_id.clone() else {
                    return;
                };
                let title = match &item.series_name {
                    Some(s) => format!("{s} › {}", item.name),
                    None => item.name.clone(),
                };
                (
                    title,
                    Source::Episodes {
                        series_id,
                        season_id: item.id.clone(),
                    },
                )
            }
            _ if item.is_playable() => {
                self.info("Press d to download this item");
                return;
            }
            _ => (
                item.name.clone(),
                Source::Children {
                    parent_id: item.id.clone(),
                    collection_type: item.collection_type.clone(),
                },
            ),
        };
        // Breadcrumb: "Library › Some folder".
        let title = match (self.tab, self.lib_stack.last()) {
            (Tab::Library, Some(parent))
                if self.lib_stack.len() > 1 && item.item_type != "Season" =>
            {
                format!("{} › {title}", parent.title)
            }
            _ => title,
        };
        let mut view = self.new_view(title, source);
        view.sort = self.sort;
        view.new_since = self.wanted_filter(&view);
        self.lib_stack.push(view);
        self.tab = Tab::Library;
        self.load(self.lib_stack.len() - 1, 0, LoadKind::Replace, None);
    }

    fn on_downloads_key(&mut self, k: KeyEvent) {
        let n = self.jobs.len();
        let sel = self.dl_state.selected();
        let job = sel.and_then(|i| self.jobs.get(i));
        match k.code {
            KeyCode::Up | KeyCode::Char('k') if n > 0 => self
                .dl_state
                .select(Some(sel.unwrap_or(0).saturating_sub(1))),
            KeyCode::Down | KeyCode::Char('j') if n > 0 => self
                .dl_state
                .select(Some((sel.unwrap_or(0) + 1).min(n - 1))),
            KeyCode::Home | KeyCode::Char('g') if n > 0 => self.dl_state.select(Some(0)),
            KeyCode::End | KeyCode::Char('G') if n > 0 => self.dl_state.select(Some(n - 1)),
            KeyCode::PageUp if n > 0 => self
                .dl_state
                .select(Some(sel.unwrap_or(0).saturating_sub(PAGE_STEP))),
            KeyCode::PageDown if n > 0 => self
                .dl_state
                .select(Some((sel.unwrap_or(0) + PAGE_STEP).min(n - 1))),
            KeyCode::Char('p') | KeyCode::Char(' ') => {
                if let Some(j) = job {
                    match j.state {
                        JobState::Paused => self.mgr.send(Command::Resume(j.id)),
                        JobState::Queued | JobState::Running => self.mgr.send(Command::Pause(j.id)),
                        _ => {}
                    }
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(j) = job {
                    self.mgr.send(Command::Cancel(j.id));
                }
            }
            KeyCode::Char('r') => {
                if let Some(j) = job {
                    self.mgr.send(Command::Retry(j.id));
                }
            }
            KeyCode::Char('P') => {
                for j in self.jobs.iter().filter(|j| j.state.is_active()) {
                    self.mgr.send(Command::Pause(j.id));
                }
            }
            KeyCode::Char('R') => {
                for j in self.jobs.iter().filter(|j| j.state == JobState::Paused) {
                    self.mgr.send(Command::Resume(j.id));
                }
            }
            KeyCode::Char('c') => self.mgr.send(Command::ClearFinished),
            KeyCode::Char('+') | KeyCode::Char('=') => self
                .mgr
                .send(Command::SetMaxParallel(self.max_parallel + 1)),
            KeyCode::Char('-') => self.mgr.send(Command::SetMaxParallel(
                self.max_parallel.saturating_sub(1).max(1),
            )),
            _ => {}
        }
    }
}

pub fn on_disk(target: &std::path::Path, expected: Option<u64>) -> bool {
    std::fs::metadata(target).is_ok_and(|m| expected.is_none_or(|e| e == m.len()))
}

/// The complete file of `it` on disk, where jellydl saves it now or where
/// older versions saved it.
pub fn downloaded_path(it: &BaseItem, root: &std::path::Path) -> Option<PathBuf> {
    let expected = it.size();
    std::iter::once(paths::target_for(it, root))
        .chain(paths::older_targets_for(it, root))
        .find(|p| on_disk(p, expected))
}

/// Run the TUI until the user quits.
pub async fn run(
    mut app: App,
    mut rx: mpsc::UnboundedReceiver<AppEvent>,
    posters: bool,
) -> Result<()> {
    let mut terminal = ratatui::init();
    if posters {
        // Ask the terminal which image protocol it supports. This reads stdin,
        // so it must run before the key reader starts.
        app.picker = super::images::detect();
    }
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let res: Result<()> = async {
        loop {
            terminal.draw(|f| super::draw::draw(f, &mut app))?;
            tokio::select! {
                ev = input.next() => match ev {
                    Some(Ok(TermEvent::Key(k))) => app.on_key(k),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                Some(ev) = rx.recv() => {
                    app.on_event(ev);
                    // Handle bursts of progress events before drawing again.
                    while let Ok(ev) = rx.try_recv() {
                        app.on_event(ev);
                    }
                }
                _ = tick.tick() => app.on_tick(),
            }
            if app.quit {
                break;
            }
        }
        Ok(())
    }
    .await;
    ratatui::restore();
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::MediaSource;

    #[test]
    fn finds_files_under_old_names() {
        let dir = tempfile::tempdir().unwrap();
        let it = BaseItem {
            id: "1".into(),
            name: "Élodie".into(),
            item_type: "Movie".into(),
            production_year: Some(2001),
            media_sources: Some(vec![MediaSource {
                size: Some(3),
                container: Some("mkv".into()),
                path: None,
            }]),
            ..Default::default()
        };
        assert_eq!(downloaded_path(&it, dir.path()), None);

        let [dotted, old] = paths::older_targets_for(&it, dir.path());
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, b"abc").unwrap();
        assert_eq!(downloaded_path(&it, dir.path()), Some(old.clone()));

        // A file from the dotted layout with type folders is found first.
        std::fs::create_dir_all(dotted.parent().unwrap()).unwrap();
        std::fs::write(&dotted, b"abc").unwrap();
        assert_eq!(downloaded_path(&it, dir.path()), Some(dotted.clone()));
        std::fs::remove_file(&dotted).unwrap();

        // A partial file (wrong size) is not a download.
        std::fs::write(&old, b"ab").unwrap();
        assert_eq!(downloaded_path(&it, dir.path()), None);

        let new = paths::target_for(&it, dir.path());
        assert_eq!(new, dir.path().join("Elodie.2001/Elodie.2001.mkv"));
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&new, b"abc").unwrap();
        assert_eq!(downloaded_path(&it, dir.path()), Some(new));
    }
}
