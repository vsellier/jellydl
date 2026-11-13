//! Web interface: `jellydl --serve`.
//!
//! A JSON API over the same Jellyfin client, download queue and folder layout
//! as the terminal interface, and one embedded page that uses it.
//! There is no login: listen on a local address, or put a reverse proxy with
//! authentication in front.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

use crate::api::{BaseItem, Jellyfin, LatestKind, Sort, SortField, with_latest_entries};
use crate::download::plan::{Plan, plan};
use crate::download::{Command, Event as MgrEvent, Job, JobState, ManagerHandle, Totals};
use crate::paths::{self, DownloadLayout, SeriesInfo, downloaded_path};

const PAGE_SIZE: usize = 100;
const POSTER_HEIGHT: u32 = 600;
const INDEX_HTML: &str = include_str!("index.html");

/// What the handlers share.
pub struct WebState {
    api: Jellyfin,
    mgr: ManagerHandle,
    layout: DownloadLayout,
    /// Last known state of each download, by id.
    jobs: Mutex<BTreeMap<u64, Job>>,
    max_parallel: Mutex<usize>,
    /// Year and status of the series seen, by id.
    series: Mutex<HashMap<String, SeriesInfo>>,
    /// Download changes, as JSON, for the open pages.
    events: broadcast::Sender<String>,
}

type Shared = Arc<WebState>;

/// Start the web server. It stops on Ctrl+C or SIGTERM.
pub async fn serve(
    api: Jellyfin,
    mgr: ManagerHandle,
    mgr_events: mpsc::UnboundedReceiver<MgrEvent>,
    layout: DownloadLayout,
    max_parallel: usize,
    listen: SocketAddr,
) -> Result<()> {
    let app = app(api, mgr, mgr_events, layout, max_parallel);
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("cannot listen on {listen}"))?;
    eprintln!("jellydl web interface: http://{listen}/");
    tracing::info!("web interface on http://{listen}/");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("web server failed")
}

/// The web application: routes, and a task that follows the download queue.
pub fn app(
    api: Jellyfin,
    mgr: ManagerHandle,
    mut mgr_events: mpsc::UnboundedReceiver<MgrEvent>,
    layout: DownloadLayout,
    max_parallel: usize,
) -> Router {
    let (events, _) = broadcast::channel(1024);
    let state = Arc::new(WebState {
        api,
        mgr,
        layout,
        jobs: Mutex::new(BTreeMap::new()),
        max_parallel: Mutex::new(max_parallel),
        series: Mutex::new(HashMap::new()),
        events,
    });

    // Keep the job list up to date and tell the open pages.
    let st = state.clone();
    tokio::spawn(async move {
        while let Some(ev) = mgr_events.recv().await {
            let msg = match ev {
                MgrEvent::Updated(job) => {
                    let msg = serde_json::json!({ "type": "job", "job": JobView::from(&job) });
                    st.jobs.lock().unwrap().insert(job.id, job);
                    msg
                }
                MgrEvent::Removed(id) => {
                    st.jobs.lock().unwrap().remove(&id);
                    serde_json::json!({ "type": "removed", "id": id })
                }
                MgrEvent::MaxParallel(n) => {
                    *st.max_parallel.lock().unwrap() = n;
                    serde_json::json!({ "type": "parallel", "value": n })
                }
            };
            let _ = st.events.send(msg.to_string());
        }
    });
    router(state)
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        let term = async {
            match term.as_mut() {
                Some(t) => {
                    t.recv().await;
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = term => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}

fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/list", get(list))
        .route("/api/items/{id}/poster", get(poster))
        .route("/api/items/{id}/people", get(people))
        .route("/api/download", post(download))
        .route("/api/jobs", get(jobs))
        .route("/api/jobs/{id}/{action}", post(job_action))
        .route("/api/queue/{action}", post(queue_action))
        .route("/api/events", get(events))
        .with_state(state)
}

// ------------------------------------------------------------------- errors

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::BAD_GATEWAY, format!("{e:#}"))
    }
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

type ApiResult<T> = Result<T, ApiError>;

// -------------------------------------------------------------------- items

/// How the page opens an item: the query to send to `/api/list`.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OpenSpec {
    src: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ct: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series: Option<String>,
    /// True when the list can be sorted and comes in pages.
    paged: bool,
}

/// An item as the page shows it.
#[derive(Debug, Serialize)]
pub struct WebItem {
    id: String,
    name: String,
    label: String,
    #[serde(rename = "type")]
    item_type: String,
    tag: &'static str,
    year: Option<i32>,
    overview: Option<String>,
    added: Option<String>,
    released: Option<String>,
    runtime_min: Option<i64>,
    rating: Option<f32>,
    size: Option<u64>,
    format: Option<String>,
    child_count: Option<i32>,
    is_4k: bool,
    /// The item has a file to download.
    playable: bool,
    /// The item holds other items (`d` downloads all of them).
    container: bool,
    /// The item has a poster (not a library or a virtual entry).
    poster: bool,
    /// Where the complete file is on disk.
    downloaded: Option<String>,
    /// Where the file goes when it is downloaded.
    target: Option<String>,
    open: Option<OpenSpec>,
}

fn open_spec(it: &BaseItem) -> Option<OpenSpec> {
    if let Some(kind) = LatestKind::from_entry(it) {
        return Some(OpenSpec {
            src: "latest",
            id: Some(latest_key(kind).into()),
            ct: None,
            series: None,
            paged: false,
        });
    }
    match it.item_type.as_str() {
        "Series" => Some(OpenSpec {
            src: "seasons",
            id: Some(it.id.clone()),
            ct: None,
            series: None,
            paged: false,
        }),
        "Season" => Some(OpenSpec {
            src: "episodes",
            id: Some(it.id.clone()),
            ct: None,
            series: Some(it.series_id.clone()?),
            paged: false,
        }),
        _ if it.is_playable() => None,
        _ => Some(OpenSpec {
            src: "children",
            id: Some(it.id.clone()),
            ct: it.collection_type.clone(),
            series: None,
            paged: true,
        }),
    }
}

fn latest_key(kind: LatestKind) -> &'static str {
    match kind {
        LatestKind::Movies => "movies",
        LatestKind::Movies4k => "movies4k",
        LatestKind::Series => "series",
    }
}

fn latest_kind(key: &str) -> Option<LatestKind> {
    LatestKind::ALL.into_iter().find(|k| latest_key(*k) == key)
}

impl WebState {
    fn web_item(&self, it: BaseItem) -> WebItem {
        let series = self.series.lock().unwrap();
        let info = it.series_id.as_ref().and_then(|id| series.get(id));
        let playable = it.is_playable();
        let downloaded = playable
            .then(|| downloaded_path(&it, &self.layout, info))
            .flatten();
        let target = (playable && downloaded.is_none())
            .then(|| self.layout.target(&it, info).display().to_string());
        WebItem {
            label: it.label(),
            tag: it.kind_tag(),
            added: it.date_added().map(str::to_string),
            released: it.release_date().map(str::to_string),
            runtime_min: it.runtime_minutes(),
            size: it.size(),
            format: playable.then(|| paths::extension(&it)),
            is_4k: it.is_4k(),
            playable,
            container: !playable && !it.is_latest_entry(),
            poster: !it.is_library(),
            downloaded: downloaded.map(|p| p.display().to_string()),
            target,
            open: open_spec(&it),
            id: it.id,
            name: it.name,
            item_type: it.item_type,
            year: it.production_year,
            overview: it.overview,
            rating: it.community_rating,
            child_count: it.child_count,
        }
    }

    /// Ask the server for the series of these items that are not known yet.
    async fn learn_series(&self, items: &[BaseItem]) {
        let missing: Vec<String> = {
            let known = self.series.lock().unwrap();
            let mut ids: Vec<String> = items
                .iter()
                .filter(|i| i.item_type == "Episode")
                .filter_map(|i| i.series_id.clone())
                .filter(|id| !known.contains_key(id))
                .collect();
            ids.sort();
            ids.dedup();
            ids
        };
        if missing.is_empty() {
            return;
        }
        match self.api.series_info(&missing).await {
            Ok(found) => self.series.lock().unwrap().extend(found),
            Err(e) => tracing::warn!("cannot read the series of the episodes: {e:#}"),
        }
    }
}

// --------------------------------------------------------------------- list

#[derive(Debug, Deserialize)]
struct ListQuery {
    src: String,
    id: Option<String>,
    ct: Option<String>,
    series: Option<String>,
    q: Option<String>,
    #[serde(default)]
    start: usize,
    sort: Option<SortField>,
    #[serde(default)]
    desc: bool,
}

#[derive(Debug, Serialize)]
struct ListReply {
    items: Vec<WebItem>,
    total: usize,
    start: usize,
}

async fn list(State(st): State<Shared>, Query(q): Query<ListQuery>) -> ApiResult<Json<ListReply>> {
    let need =
        |v: Option<String>, name: &str| v.ok_or_else(|| bad_request(format!("{name} is missing")));
    let sort = Sort {
        field: q.sort.unwrap_or_default(),
        descending: q.desc,
    };
    let r = match q.src.as_str() {
        "libraries" => {
            let items = with_latest_entries(st.api.views().await?);
            crate::api::QueryResult {
                total_record_count: items.len(),
                items,
            }
        }
        "children" => {
            let id = need(q.id, "id")?;
            st.api
                .children(&id, q.ct.as_deref(), sort, q.start, PAGE_SIZE)
                .await?
        }
        "seasons" => st.api.seasons(&need(q.id, "id")?).await?,
        "episodes" => {
            st.api
                .episodes(&need(q.series, "series")?, &need(q.id, "id")?)
                .await?
        }
        "search" => {
            let term = need(q.q, "q")?;
            if term.trim().is_empty() {
                crate::api::QueryResult::default()
            } else {
                st.api.search(term.trim()).await?
            }
        }
        "latest" => {
            let key = need(q.id, "id")?;
            let kind = latest_kind(&key).ok_or_else(|| bad_request("unknown latest list"))?;
            st.api.latest(kind).await?
        }
        other => return Err(bad_request(format!("unknown source {other}"))),
    };
    st.learn_series(&r.items).await;
    let total = r.total_record_count;
    let items = r.items.into_iter().map(|it| st.web_item(it)).collect();
    Ok(Json(ListReply {
        items,
        total,
        start: q.start,
    }))
}

// ---------------------------------------------------------- poster, people

async fn poster(State(st): State<Shared>, Path(id): Path<String>) -> ApiResult<Response> {
    let Some(bytes) = st.api.primary_image(&id, POSTER_HEIGHT).await? else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let mime = match bytes.get(..4) {
        Some([0x89, b'P', b'N', b'G']) => "image/png",
        Some(b"RIFF") => "image/webp",
        _ => "image/jpeg",
    };
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "private, max-age=86400"),
        ],
        bytes,
    )
        .into_response())
}

async fn people(
    State(st): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<crate::api::Person>>> {
    Ok(Json(st.api.people(&id).await?))
}

// ----------------------------------------------------------------- download

#[derive(Debug, Deserialize)]
struct DownloadRequest {
    ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DownloadReply {
    queued: usize,
    skipped: usize,
    size: u64,
    unknown_size: usize,
}

async fn download(
    State(st): State<Shared>,
    Json(req): Json<DownloadRequest>,
) -> ApiResult<Json<DownloadReply>> {
    let ids: Vec<String> = req
        .ids
        .into_iter()
        .filter(|id| !id.starts_with("jellydl:"))
        .collect();
    if ids.is_empty() {
        return Err(bad_request("nothing to download"));
    }
    let targets = st.api.items_by_ids(&ids).await?;
    let (mut all, folders): (Vec<_>, Vec<_>) = targets.into_iter().partition(|i| i.is_playable());
    for f in folders {
        let inside = st
            .api
            .descendants(&f.id)
            .await
            .with_context(|| f.name.clone())?;
        all.extend(inside);
    }
    st.learn_series(&all).await;
    let Plan {
        jobs,
        skipped,
        size,
        unknown_size,
    } = {
        let jobs = st.jobs.lock().unwrap();
        let busy: HashSet<&str> = jobs
            .values()
            .filter(|j| {
                matches!(
                    j.state,
                    JobState::Queued | JobState::Running | JobState::Paused
                )
            })
            .map(|j| j.item_id.as_str())
            .collect();
        let series = st.series.lock().unwrap();
        plan(all, &busy, &st.layout, &series, |id| {
            st.api.download_url(id)
        })
    };
    let queued = jobs.len();
    if queued > 0 {
        st.mgr.send(Command::Enqueue(jobs));
    }
    Ok(Json(DownloadReply {
        queued,
        skipped,
        size,
        unknown_size,
    }))
}

// --------------------------------------------------------------------- jobs

#[derive(Debug, Serialize)]
pub struct JobView {
    id: u64,
    item_id: String,
    title: String,
    state: JobState,
    downloaded: u64,
    total: Option<u64>,
    speed: f64,
    eta: Option<u64>,
    error: Option<String>,
    target: String,
    retry: u32,
}

impl From<&Job> for JobView {
    fn from(j: &Job) -> Self {
        Self {
            id: j.id,
            item_id: j.item_id.clone(),
            title: j.title.clone(),
            state: j.state,
            downloaded: j.downloaded,
            total: j.total.or(j.expected_size),
            speed: j.speed,
            eta: j.eta_secs(),
            error: j.error.clone(),
            target: j.target.display().to_string(),
            retry: j.retry,
        }
    }
}

#[derive(Debug, Serialize)]
struct JobsReply {
    jobs: Vec<JobView>,
    max_parallel: usize,
    remaining: u64,
}

async fn jobs(State(st): State<Shared>) -> Json<JobsReply> {
    let jobs = st.jobs.lock().unwrap();
    Json(JobsReply {
        jobs: jobs.values().map(JobView::from).collect(),
        max_parallel: *st.max_parallel.lock().unwrap(),
        remaining: Totals::of(jobs.values()).remaining,
    })
}

async fn job_action(
    State(st): State<Shared>,
    Path((id, action)): Path<(u64, String)>,
) -> ApiResult<StatusCode> {
    if !st.jobs.lock().unwrap().contains_key(&id) {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such download".into()));
    }
    let cmd = match action.as_str() {
        "pause" => Command::Pause(id),
        "resume" => Command::Resume(id),
        "cancel" | "remove" => Command::Cancel(id),
        "retry" => Command::Retry(id),
        _ => return Err(bad_request("unknown action")),
    };
    st.mgr.send(cmd);
    Ok(StatusCode::NO_CONTENT)
}

async fn queue_action(
    State(st): State<Shared>,
    Path(action): Path<String>,
) -> ApiResult<StatusCode> {
    let ids_in = |states: &[JobState]| -> Vec<u64> {
        st.jobs
            .lock()
            .unwrap()
            .values()
            .filter(|j| states.contains(&j.state))
            .map(|j| j.id)
            .collect()
    };
    match action.as_str() {
        "pause-all" => {
            for id in ids_in(&[JobState::Queued, JobState::Running]) {
                st.mgr.send(Command::Pause(id));
            }
        }
        "resume-all" => {
            for id in ids_in(&[JobState::Paused]) {
                st.mgr.send(Command::Resume(id));
            }
        }
        "clear" => st.mgr.send(Command::ClearFinished),
        "more" | "less" => {
            let n = *st.max_parallel.lock().unwrap();
            let n = if action == "more" {
                n + 1
            } else {
                n.saturating_sub(1)
            };
            st.mgr.send(Command::SetMaxParallel(
                n.clamp(1, crate::config::MAX_PARALLEL_LIMIT),
            ));
        }
        _ => return Err(bad_request("unknown action")),
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Download changes as server-sent events. The page reloads the full list when
/// it connects, so a lost event is fixed at the next connection.
async fn events(State(st): State<Shared>) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = st.events.subscribe();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        let msg = match rx.recv().await {
            Ok(msg) => msg,
            // Too slow: some events were dropped. Ask the page to reload.
            Err(broadcast::error::RecvError::Lagged(_)) => {
                serde_json::json!({ "type": "reload" }).to_string()
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        };
        Some((Ok(SseEvent::default().data(msg)), rx))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn index() -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-cache")], Html(INDEX_HTML))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_like_the_terminal() {
        let item = |t: &str| BaseItem {
            id: "i".into(),
            item_type: t.into(),
            series_id: Some("s".into()),
            collection_type: Some("movies".into()),
            ..Default::default()
        };
        assert_eq!(open_spec(&item("Series")).unwrap().src, "seasons");
        let season = open_spec(&item("Season")).unwrap();
        assert_eq!(
            (season.src, season.series.as_deref()),
            ("episodes", Some("s"))
        );
        let lib = open_spec(&item("CollectionFolder")).unwrap();
        assert_eq!(
            (lib.src, lib.ct.as_deref(), lib.paged),
            ("children", Some("movies"), true)
        );
        assert_eq!(open_spec(&item("Movie")), None);
        let latest = open_spec(&LatestKind::Movies4k.entry()).unwrap();
        assert_eq!(
            (latest.src, latest.id.as_deref()),
            ("latest", Some("movies4k"))
        );
        assert_eq!(latest_kind("series"), Some(LatestKind::Series));
    }
}
