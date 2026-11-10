//! A navigable list of Jellyfin items (a library, a series, search results...).

use std::collections::HashSet;

use ratatui::widgets::ListState;

use crate::api::{BaseItem, Sort};

pub const PAGE_SIZE: usize = 100;

#[derive(Debug, Clone)]
pub enum Source {
    Libraries,
    Children {
        parent_id: String,
        collection_type: Option<String>,
    },
    Seasons {
        series_id: String,
    },
    Episodes {
        series_id: String,
        season_id: String,
    },
    Search {
        term: String,
    },
}

impl Source {
    pub fn is_paged(&self) -> bool {
        matches!(self, Source::Children { .. })
    }
}

/// How a loaded page joins the items already in the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadKind {
    /// Drop the loaded items. The page becomes the new window.
    Replace,
    /// Add the page after the loaded items.
    Append,
    /// Add the page before the loaded items.
    Prepend,
}

#[derive(Debug, Clone, Copy)]
pub struct Pending {
    pub req: u64,
    pub start: usize,
    pub kind: LoadKind,
    /// Absolute index to select when the page is in.
    pub select: Option<usize>,
}

/// What to do to show a page.
#[derive(Debug, PartialEq, Eq)]
pub enum PageAction {
    /// The page is loaded: its first item is selected.
    Shown,
    /// Load `start` with this kind, then select `start`.
    Load { start: usize, kind: LoadKind },
}

/// A list of items. For paged sources, `items` is a window of the full list
/// that starts at the absolute index `offset`.
pub struct ListView {
    pub id: u64,
    pub title: String,
    pub source: Source,
    pub sort: Sort,
    pub items: Vec<BaseItem>,
    pub offset: usize,
    pub total: usize,
    pub state: ListState,
    pub pending: Option<Pending>,
    pub error: Option<String>,
    next_req: u64,
    marked: HashSet<String>,
    /// Marked items in mark order. They stay marked when the window moves.
    marked_items: Vec<BaseItem>,
}

impl ListView {
    pub fn new(id: u64, title: impl Into<String>, source: Source) -> Self {
        Self {
            id,
            title: title.into(),
            source,
            sort: Sort::default(),
            items: Vec::new(),
            offset: 0,
            total: 0,
            state: ListState::default(),
            pending: None,
            error: None,
            next_req: 0,
            marked: HashSet::new(),
            marked_items: Vec::new(),
        }
    }

    pub fn loading(&self) -> bool {
        self.pending.is_some()
    }

    pub fn selected(&self) -> Option<&BaseItem> {
        self.items.get(self.state.selected()?)
    }

    /// Move the selection by `delta`, clamped to the list.
    pub fn move_by(&mut self, delta: isize) {
        if self.items.is_empty() {
            self.state.select(None);
            return;
        }
        let cur = self.state.selected().unwrap_or(0) as isize;
        let max = self.items.len() as isize - 1;
        self.state
            .select(Some((cur + delta).clamp(0, max) as usize));
    }

    pub fn select_first(&mut self) {
        if !self.items.is_empty() {
            self.state.select(Some(0));
        }
    }

    pub fn select_last(&mut self) {
        if !self.items.is_empty() {
            self.state.select(Some(self.items.len() - 1));
        }
    }

    // ---------------------------------------------------------------- loading

    /// Record a new request. Replies to older requests are then ignored.
    pub fn begin_load(&mut self, start: usize, kind: LoadKind, select: Option<usize>) -> u64 {
        self.next_req += 1;
        self.pending = Some(Pending {
            req: self.next_req,
            start,
            kind,
            select,
        });
        self.error = None;
        self.next_req
    }

    /// Add a loaded page. Returns false for a reply to an old request.
    pub fn apply_page(&mut self, req: u64, items: Vec<BaseItem>, total: usize) -> bool {
        let Some(p) = self.pending.filter(|p| p.req == req) else {
            return false;
        };
        self.pending = None;
        let n = items.len();
        match p.kind {
            LoadKind::Replace => {
                self.items = items;
                self.offset = p.start;
                self.state = ListState::default();
            }
            LoadKind::Append if p.start == self.offset + self.items.len() => {
                self.items.extend(items);
            }
            LoadKind::Prepend if p.start + n == self.offset => {
                self.items.splice(0..0, items);
                self.offset = p.start;
                if let Some(sel) = self.state.selected() {
                    self.state.select(Some(sel + n));
                }
                *self.state.offset_mut() += n;
            }
            // The window changed since the request: the page does not fit.
            LoadKind::Append | LoadKind::Prepend => {}
        }
        self.total = total.max(self.offset + self.items.len());
        match p.select {
            Some(abs) => self.show_abs(abs),
            None if self.state.selected().is_none() => self.select_first(),
            None => {}
        }
        true
    }

    /// Record a failed request. Returns false for a reply to an old request.
    pub fn fail(&mut self, req: u64, err: String) -> bool {
        if self.pending.is_none_or(|p| p.req != req) {
            return false;
        }
        self.pending = None;
        self.error = Some(err);
        true
    }

    /// Select the item at absolute index `abs` and scroll it to the top.
    fn show_abs(&mut self, abs: usize) {
        if abs >= self.offset && abs < self.offset + self.items.len() {
            let i = abs - self.offset;
            self.state.select(Some(i));
            *self.state.offset_mut() = i;
        } else {
            self.select_first();
        }
    }

    /// True when the user is near the end of the loaded items and more exist.
    pub fn wants_next_page(&self) -> bool {
        self.source.is_paged()
            && !self.loading()
            && self.offset + self.items.len() < self.total
            && self.state.selected().unwrap_or(0) + 20 >= self.items.len()
    }

    /// True when the user is near the start of the loaded items and some come before.
    pub fn wants_prev_page(&self) -> bool {
        self.source.is_paged()
            && !self.loading()
            && self.offset > 0
            && self.state.selected().unwrap_or(0) < 20
    }

    // ------------------------------------------------------------------ pages

    pub fn page_count(&self) -> usize {
        self.total.div_ceil(PAGE_SIZE).max(1)
    }

    /// Page (0-based) of the selected item.
    pub fn current_page(&self) -> usize {
        (self.offset + self.state.selected().unwrap_or(0)) / PAGE_SIZE
    }

    /// Go to `page` (0-based, clamped). Selects it now when it is loaded,
    /// else says which page to load.
    pub fn goto_page(&mut self, page: usize) -> PageAction {
        let page = page.min(self.page_count() - 1);
        let start = page * PAGE_SIZE;
        let end = self.offset + self.items.len();
        if !self.source.is_paged() || (start >= self.offset && start < end) {
            self.show_abs(start);
            return PageAction::Shown;
        }
        let kind = if start == end {
            LoadKind::Append
        } else if start + PAGE_SIZE == self.offset {
            LoadKind::Prepend
        } else {
            LoadKind::Replace
        };
        PageAction::Load { start, kind }
    }

    // ------------------------------------------------------------------ marks

    pub fn is_marked(&self, id: &str) -> bool {
        self.marked.contains(id)
    }

    pub fn marked_count(&self) -> usize {
        self.marked.len()
    }

    pub fn clear_marks(&mut self) {
        self.marked.clear();
        self.marked_items.clear();
    }

    fn set_mark(&mut self, item: &BaseItem, on: bool) {
        if on {
            if self.marked.insert(item.id.clone()) {
                self.marked_items.push(item.clone());
            }
        } else if self.marked.remove(&item.id) {
            self.marked_items.retain(|i| i.id != item.id);
        }
    }

    pub fn toggle_mark(&mut self) {
        if let Some(it) = self.selected().cloned() {
            let on = !self.is_marked(&it.id);
            self.set_mark(&it, on);
        }
    }

    /// Mark all loaded items, or clear every mark when they are all marked.
    pub fn toggle_mark_all(&mut self) {
        if self.items.iter().all(|i| self.marked.contains(&i.id)) {
            self.clear_marks();
        } else {
            for it in self.items.clone() {
                self.set_mark(&it, true);
            }
        }
    }

    /// Marked items, or the selected one when nothing is marked.
    pub fn targets(&self) -> Vec<BaseItem> {
        if self.marked.is_empty() {
            self.selected().cloned().into_iter().collect()
        } else {
            self.marked_items.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paged() -> ListView {
        ListView::new(
            1,
            "Movies",
            Source::Children {
                parent_id: "p".into(),
                collection_type: Some("movies".into()),
            },
        )
    }

    fn page(start: usize, n: usize) -> Vec<BaseItem> {
        (start..start + n)
            .map(|i| BaseItem {
                id: format!("i{i}"),
                name: format!("Item {i}"),
                ..Default::default()
            })
            .collect()
    }

    fn load(v: &mut ListView, start: usize, kind: LoadKind, total: usize) {
        let req = v.begin_load(start, kind, Some(start));
        let n = PAGE_SIZE.min(total - start);
        assert!(v.apply_page(req, page(start, n), total));
    }

    fn selected_id(v: &ListView) -> String {
        v.selected().unwrap().id.clone()
    }

    #[test]
    fn next_and_previous_page_extend_the_window() {
        let mut v = paged();
        load(&mut v, 0, LoadKind::Replace, 250);
        assert_eq!((v.page_count(), v.current_page()), (3, 0));

        let PageAction::Load { start, kind } = v.goto_page(1) else {
            panic!("page 2 is not loaded yet")
        };
        assert_eq!((start, kind), (100, LoadKind::Append));
        load(&mut v, start, kind, 250);
        assert_eq!(v.items.len(), 200);
        assert_eq!(selected_id(&v), "i100");
        assert_eq!(v.current_page(), 1);

        // Back to page 1: it is loaded, no request.
        assert_eq!(v.goto_page(0), PageAction::Shown);
        assert_eq!(selected_id(&v), "i0");
    }

    #[test]
    fn far_jump_replaces_the_window_and_scrolling_up_prepends() {
        let mut v = paged();
        load(&mut v, 0, LoadKind::Replace, 1000);
        let PageAction::Load { start, kind } = v.goto_page(usize::MAX) else {
            panic!()
        };
        assert_eq!((start, kind), (900, LoadKind::Replace));
        load(&mut v, start, kind, 1000);
        assert_eq!((v.offset, v.items.len(), v.current_page()), (900, 100, 9));
        assert!(v.wants_prev_page());

        let PageAction::Load { start, kind } = v.goto_page(8) else {
            panic!()
        };
        assert_eq!((start, kind), (800, LoadKind::Prepend));
        load(&mut v, start, kind, 1000);
        assert_eq!((v.offset, v.items.len()), (800, 200));
        assert_eq!(selected_id(&v), "i800");
    }

    #[test]
    fn prepend_keeps_the_selected_item() {
        let mut v = paged();
        load(&mut v, 0, LoadKind::Replace, 1000);
        load(&mut v, 500, LoadKind::Replace, 1000);
        v.move_by(3);
        let req = v.begin_load(400, LoadKind::Prepend, None);
        assert!(v.apply_page(req, page(400, 100), 1000));
        assert_eq!(selected_id(&v), "i503");
    }

    #[test]
    fn old_replies_are_ignored() {
        let mut v = paged();
        let old = v.begin_load(0, LoadKind::Replace, None);
        let new = v.begin_load(0, LoadKind::Replace, None);
        assert!(!v.apply_page(old, page(0, 1), 1));
        assert!(v.loading());
        assert!(v.apply_page(new, page(0, 2), 2));
        assert!(!v.fail(old, "late".into()));
        assert_eq!(v.items.len(), 2);
    }

    #[test]
    fn marks_survive_a_window_change() {
        let mut v = paged();
        load(&mut v, 0, LoadKind::Replace, 1000);
        v.toggle_mark();
        load(&mut v, 500, LoadKind::Replace, 1000);
        v.toggle_mark();
        let ids: Vec<_> = v.targets().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, ["i0", "i500"]);
        v.toggle_mark_all();
        assert_eq!(v.marked_count(), 101);
        v.toggle_mark_all();
        assert_eq!(v.marked_count(), 0);
    }
}
