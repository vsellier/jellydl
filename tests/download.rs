//! Download manager tests against a fake HTTP server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jellydl::download::{
    self, Command, Event, Job, JobState, ManagerConfig, ManagerHandle, NewJob,
};
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

/// Serves `data`, with Range support when `ranges` is true.
struct FileResponder {
    data: Vec<u8>,
    ranges: bool,
    delay: Duration,
}

impl Respond for FileResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let range = req
            .headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.strip_suffix('-'))
            .and_then(|v| v.parse::<usize>().ok());
        let t = match (self.ranges, range) {
            (true, Some(start)) if start < self.data.len() => ResponseTemplate::new(206)
                .insert_header(
                    "Content-Range",
                    format!("bytes {start}-{}/{}", self.data.len() - 1, self.data.len()),
                )
                .set_body_bytes(self.data[start..].to_vec()),
            (true, Some(_)) => ResponseTemplate::new(416),
            _ => ResponseTemplate::new(200).set_body_bytes(self.data.clone()),
        };
        t.set_delay(self.delay)
    }
}

struct Harness {
    mgr: ManagerHandle,
    rx: mpsc::UnboundedReceiver<Event>,
    jobs: HashMap<u64, Job>,
    max_running_seen: usize,
}

impl Harness {
    fn new(max_parallel: usize) -> Self {
        Self::with(max_parallel, None, None)
    }

    fn with(
        max_parallel: usize,
        max_speed: Option<u64>,
        start_delay: Option<(Duration, Duration)>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let mgr = download::spawn(
            ManagerConfig {
                client: reqwest::Client::new(),
                auth_header: "MediaBrowser Token=\"t\"".into(),
                max_parallel,
                queue_file: None,
                retry_base: Duration::from_millis(10),
                max_retries: 3,
                max_speed,
                start_delay,
            },
            tx,
        );
        Self {
            mgr,
            rx,
            jobs: HashMap::new(),
            max_running_seen: 0,
        }
    }

    /// Read events until `pred` is true, or panic after 10 s.
    async fn wait(&mut self, what: &str, pred: impl Fn(&HashMap<u64, Job>) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !pred(&self.jobs) {
            let ev = tokio::time::timeout_at(deadline, self.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timeout waiting for: {what}; jobs: {:#?}", self.jobs))
                .expect("manager stopped");
            match ev {
                Event::Updated(j) => {
                    self.jobs.insert(j.id, j);
                }
                Event::Removed(id) => {
                    self.jobs.remove(&id);
                }
                Event::MaxParallel(_) => {}
            }
            let running = self
                .jobs
                .values()
                .filter(|j| j.state == JobState::Running)
                .count();
            self.max_running_seen = self.max_running_seen.max(running);
        }
    }

    async fn wait_state(&mut self, id: u64, s: JobState) {
        self.wait(&format!("job {id} {s:?}"), |j| {
            j.get(&id).is_some_and(|j| j.state == s)
        })
        .await;
    }
}

fn job(server: &MockServer, name: &str, dir: &Path, size: Option<u64>) -> NewJob {
    NewJob {
        item_id: name.into(),
        title: name.into(),
        url: format!("{}/Items/{name}/Download", server.uri()),
        target: dir.join(format!("{name}.mkv")),
        expected_size: size,
    }
}

fn part(p: &Path) -> PathBuf {
    download::job::part_path(p)
}

async fn mount_file(server: &MockServer, name: &str, r: FileResponder) {
    Mock::given(method("GET"))
        .and(path(format!("/Items/{name}/Download")))
        .respond_with(r)
        .mount(server)
        .await;
}

#[tokio::test]
async fn downloads_a_file_and_sends_auth() {
    let server = MockServer::start().await;
    let body = data(300_000);
    mount_file(
        &server,
        "a",
        FileResponder {
            data: body.clone(),
            ranges: true,
            delay: Duration::ZERO,
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::new(2);
    h.mgr.send(Command::Enqueue(vec![job(
        &server,
        "a",
        dir.path(),
        Some(body.len() as u64),
    )]));
    h.wait_state(1, JobState::Done).await;

    let target = dir.path().join("a.mkv");
    assert_eq!(std::fs::read(&target).unwrap(), body);
    assert!(!part(&target).exists());
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(
        reqs[0].headers.get("authorization").unwrap(),
        "MediaBrowser Token=\"t\""
    );
}

#[tokio::test]
async fn resumes_from_part_file_with_range() {
    let server = MockServer::start().await;
    let body = data(200_000);
    mount_file(
        &server,
        "a",
        FileResponder {
            data: body.clone(),
            ranges: true,
            delay: Duration::ZERO,
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("a.mkv");
    std::fs::write(part(&target), &body[..80_000]).unwrap();

    let mut h = Harness::new(1);
    h.mgr.send(Command::Enqueue(vec![job(
        &server,
        "a",
        dir.path(),
        Some(body.len() as u64),
    )]));
    h.wait_state(1, JobState::Done).await;

    assert_eq!(std::fs::read(&target).unwrap(), body);
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].headers.get("range").unwrap(), "bytes=80000-");
}

#[tokio::test]
async fn restarts_when_server_ignores_range() {
    let server = MockServer::start().await;
    let body = data(100_000);
    mount_file(
        &server,
        "a",
        FileResponder {
            data: body.clone(),
            ranges: false,
            delay: Duration::ZERO,
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("a.mkv");
    std::fs::write(part(&target), vec![0xFFu8; 30_000]).unwrap();

    let mut h = Harness::new(1);
    h.mgr.send(Command::Enqueue(vec![job(
        &server,
        "a",
        dir.path(),
        Some(body.len() as u64),
    )]));
    h.wait_state(1, JobState::Done).await;
    assert_eq!(std::fs::read(&target).unwrap(), body);
}

#[tokio::test]
async fn respects_max_parallel() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut jobs = Vec::new();
    for i in 0..5 {
        let name = format!("f{i}");
        mount_file(
            &server,
            &name,
            FileResponder {
                data: data(1000),
                ranges: true,
                delay: Duration::from_millis(300),
            },
        )
        .await;
        jobs.push(job(&server, &name, dir.path(), Some(1000)));
    }
    let mut h = Harness::new(2);
    h.mgr.send(Command::Enqueue(jobs));
    h.wait("all done", |j| {
        j.len() == 5 && j.values().all(|j| j.state == JobState::Done)
    })
    .await;
    assert_eq!(
        h.max_running_seen, 2,
        "never more than 2, and the limit was used"
    );
}

#[tokio::test]
async fn forbidden_fails_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::new(1);
    h.mgr
        .send(Command::Enqueue(vec![job(&server, "a", dir.path(), None)]));
    h.wait_state(1, JobState::Failed).await;
    assert!(h.jobs[&1].error.as_deref().unwrap().contains("403"));
}

#[tokio::test]
async fn retries_server_errors() {
    let server = MockServer::start().await;
    let body = data(5000);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    mount_file(
        &server,
        "a",
        FileResponder {
            data: body.clone(),
            ranges: true,
            delay: Duration::ZERO,
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::new(1);
    h.mgr.send(Command::Enqueue(vec![job(
        &server,
        "a",
        dir.path(),
        Some(5000),
    )]));
    h.wait_state(1, JobState::Done).await;
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn pause_keeps_part_and_cancel_removes_it() {
    let server = MockServer::start().await;
    let body = data(50_000);
    mount_file(
        &server,
        "a",
        FileResponder {
            data: body.clone(),
            ranges: true,
            delay: Duration::from_millis(500),
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("a.mkv");
    std::fs::write(part(&target), &body[..10_000]).unwrap();

    let mut h = Harness::new(1);
    h.mgr.send(Command::Enqueue(vec![job(
        &server,
        "a",
        dir.path(),
        Some(50_000),
    )]));
    h.wait_state(1, JobState::Running).await;
    h.mgr.send(Command::Pause(1));
    h.wait_state(1, JobState::Paused).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(part(&target).exists(), "pause keeps the partial file");

    h.mgr.send(Command::Resume(1));
    h.wait_state(1, JobState::Running).await;
    h.mgr.send(Command::Cancel(1));
    h.wait_state(1, JobState::Canceled).await;
    // The file is removed once the worker has stopped.
    for _ in 0..50 {
        if !part(&target).exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!part(&target).exists(), "cancel removes the partial file");
    assert!(!target.exists());
}

#[tokio::test]
async fn skips_complete_files() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.mkv"), data(1234)).unwrap();
    let mut h = Harness::new(1);
    h.mgr.send(Command::Enqueue(vec![job(
        &server,
        "a",
        dir.path(),
        Some(1234),
    )]));
    h.wait_state(1, JobState::Done).await;
}

#[tokio::test]
async fn shutdown_saves_queue_and_requeues_running() {
    let server = MockServer::start().await;
    mount_file(
        &server,
        "a",
        FileResponder {
            data: data(1000),
            ranges: true,
            delay: Duration::from_secs(5),
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let qf = dir.path().join("queue.json");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mgr = download::spawn(
        ManagerConfig {
            client: reqwest::Client::new(),
            auth_header: String::new(),
            max_parallel: 1,
            queue_file: Some(qf.clone()),
            retry_base: Duration::from_millis(10),
            max_retries: 0,
            max_speed: None,
            start_delay: None,
        },
        tx,
    );
    mgr.send(Command::Enqueue(vec![job(
        &server,
        "a",
        dir.path(),
        Some(1000),
    )]));
    loop {
        if let Some(Event::Updated(j)) = rx.recv().await
            && j.state == JobState::Running
        {
            break;
        }
    }
    mgr.shutdown().await;
    let saved = download::persist::load(&qf).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].state, JobState::Queued);
}

#[tokio::test]
async fn speed_limit_is_shared_by_all_downloads() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut jobs = Vec::new();
    for i in 0..2 {
        let name = format!("s{i}");
        mount_file(
            &server,
            &name,
            FileResponder {
                data: data(300_000),
                ranges: true,
                delay: Duration::ZERO,
            },
        )
        .await;
        jobs.push(job(&server, &name, dir.path(), Some(300_000)));
    }
    // 600 KB in total at 1 MB/s: about 0.5 s, even with 2 downloads in parallel.
    let mut h = Harness::with(2, Some(1024 * 1024), None);
    let start = std::time::Instant::now();
    h.mgr.send(Command::Enqueue(jobs));
    h.wait("all done", |j| {
        j.len() == 2 && j.values().all(|j| j.state == JobState::Done)
    })
    .await;
    let secs = start.elapsed().as_secs_f64();
    assert!(secs > 0.4, "limit not applied: {secs} s");
}

#[tokio::test]
async fn start_delay_spaces_out_downloads() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut jobs = Vec::new();
    for i in 0..3 {
        let name = format!("d{i}");
        mount_file(
            &server,
            &name,
            FileResponder {
                data: data(100),
                ranges: true,
                delay: Duration::ZERO,
            },
        )
        .await;
        jobs.push(job(&server, &name, dir.path(), Some(100)));
    }
    let d = Duration::from_millis(250);
    let mut h = Harness::with(3, None, Some((d, d)));
    let start = std::time::Instant::now();
    h.mgr.send(Command::Enqueue(jobs));
    h.wait("all done", |j| {
        j.len() == 3 && j.values().all(|j| j.state == JobState::Done)
    })
    .await;
    let secs = start.elapsed().as_secs_f64();
    // The 1st starts at once, the 2nd after 250 ms, the 3rd after 500 ms.
    assert!(secs >= 0.5, "no delay between starts: {secs} s");
    assert_eq!(
        h.max_running_seen, 1,
        "the delay must keep the starts apart"
    );
}

#[tokio::test]
async fn cancel_removes_a_queued_download() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    for name in ["slow", "waiting"] {
        mount_file(
            &server,
            name,
            FileResponder {
                data: data(1000),
                ranges: true,
                delay: Duration::from_millis(500),
            },
        )
        .await;
    }
    // A partial file from an earlier run.
    let waiting = dir.path().join("waiting.mkv");
    std::fs::write(part(&waiting), [0u8; 10]).unwrap();
    let mut h = Harness::new(1);
    h.mgr.send(Command::Enqueue(vec![
        job(&server, "slow", dir.path(), Some(1000)),
        job(&server, "waiting", dir.path(), Some(1000)),
    ]));
    h.wait_state(1, JobState::Running).await;
    h.wait_state(2, JobState::Queued).await;
    h.mgr.send(Command::Cancel(2));
    h.wait("queued job removed", |j| !j.contains_key(&2)).await;
    h.wait_state(1, JobState::Done).await;
    assert!(!waiting.exists());
    assert!(!part(&waiting).exists(), "x removes the partial file");
}
