//! The download queue. Runs as its own task and talks to the UI through channels.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Client;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::job::{Job, JobState, NewJob};
use super::limiter::RateLimiter;
use super::persist;
use super::worker::{self, Spec, WorkerCtx, WorkerError, WorkerMsg};
use crate::config::MAX_PARALLEL_LIMIT;

pub struct ManagerConfig {
    pub client: Client,
    pub auth_header: String,
    pub max_parallel: usize,
    /// Where to save the queue. `None` disables saving (tests).
    pub queue_file: Option<PathBuf>,
    pub retry_base: Duration,
    pub max_retries: u32,
    /// Total speed limit in bytes per second.
    pub max_speed: Option<u64>,
    /// Random wait (min, max) between two download starts.
    pub start_delay: Option<(Duration, Duration)>,
}

#[derive(Debug)]
pub enum Command {
    Enqueue(Vec<NewJob>),
    Pause(u64),
    Resume(u64),
    Retry(u64),
    Cancel(u64),
    SetMaxParallel(usize),
    ClearFinished,
    Shutdown(oneshot::Sender<()>),
}

#[derive(Debug, Clone)]
pub enum Event {
    Updated(Job),
    Removed(u64),
    MaxParallel(usize),
}

#[derive(Clone)]
pub struct ManagerHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl ManagerHandle {
    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }

    /// Stop running downloads (they stay queued), save the queue, and wait.
    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Command::Shutdown(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Stop {
    Pause,
    Cancel,
    Requeue,
}

struct Running {
    token: CancellationToken,
    stop: Option<Stop>,
}

struct Manager {
    jobs: Vec<Job>,
    next_id: u64,
    running: HashMap<u64, Running>,
    max_parallel: usize,
    events: mpsc::UnboundedSender<Event>,
    worker_tx: mpsc::UnboundedSender<WorkerMsg>,
    ctx: Arc<WorkerCtx>,
    queue_file: Option<PathBuf>,
    dirty: bool,
    start_delay: Option<(Duration, Duration)>,
    /// No new download starts before this time.
    next_start: Instant,
}

pub fn spawn(cfg: ManagerConfig, events: mpsc::UnboundedSender<Event>) -> ManagerHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let (worker_tx, worker_rx) = mpsc::unbounded_channel();

    let jobs = match &cfg.queue_file {
        Some(f) => persist::load(f).unwrap_or_else(|e| {
            tracing::error!("cannot load the saved queue: {e:#}");
            Vec::new()
        }),
        None => Vec::new(),
    };
    let next_id = jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let mut m = Manager {
        jobs,
        next_id,
        running: HashMap::new(),
        max_parallel: cfg.max_parallel.clamp(1, MAX_PARALLEL_LIMIT),
        events,
        worker_tx,
        ctx: Arc::new(WorkerCtx {
            client: cfg.client,
            auth_header: cfg.auth_header,
            retry_base: cfg.retry_base,
            max_retries: cfg.max_retries,
            limiter: cfg.max_speed.map(|b| Arc::new(RateLimiter::new(b))),
        }),
        queue_file: cfg.queue_file,
        start_delay: cfg.start_delay,
        next_start: Instant::now(),
        dirty: false,
    };
    for j in &m.jobs {
        let _ = m.events.send(Event::Updated(j.clone()));
    }
    let _ = m.events.send(Event::MaxParallel(m.max_parallel));
    tokio::spawn(async move { m.run(rx, worker_rx).await });
    ManagerHandle { tx }
}

impl Manager {
    async fn run(
        &mut self,
        mut cmds: mpsc::UnboundedReceiver<Command>,
        mut worker_rx: mpsc::UnboundedReceiver<WorkerMsg>,
    ) {
        let mut save_tick = tokio::time::interval(Duration::from_secs(1));
        self.schedule();
        loop {
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(Command::Shutdown(reply)) => {
                        self.shutdown(&mut worker_rx).await;
                        let _ = reply.send(());
                        return;
                    }
                    Some(c) => self.handle(c),
                    None => {
                        self.shutdown(&mut worker_rx).await;
                        return;
                    }
                },
                Some(msg) = worker_rx.recv() => self.on_worker(msg),
                _ = save_tick.tick() => self.save_if_dirty(),
                // Wake up when the start delay is over.
                _ = tokio::time::sleep_until(self.next_start.into()), if self.waiting_for_delay() => {}
            }
            self.schedule();
        }
    }

    async fn shutdown(&mut self, worker_rx: &mut mpsc::UnboundedReceiver<WorkerMsg>) {
        for r in self.running.values_mut() {
            r.stop.get_or_insert(Stop::Requeue);
            r.token.cancel();
        }
        let deadline = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline);
        while !self.running.is_empty() {
            tokio::select! {
                Some(msg) = worker_rx.recv() => self.on_worker(msg),
                _ = &mut deadline => break,
            }
        }
        self.dirty = true;
        self.save_if_dirty();
    }

    fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        if let Some(f) = &self.queue_file
            && let Err(e) = persist::save(f, &self.jobs)
        {
            tracing::error!("cannot save the queue: {e:#}");
        }
    }

    fn job_mut(&mut self, id: u64) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    /// Send the job to the UI.
    fn notify(&self, id: u64) {
        if let Some(j) = self.jobs.iter().find(|j| j.id == id) {
            let _ = self.events.send(Event::Updated(j.clone()));
        }
    }

    /// Send the job to the UI, and save the queue at the next tick.
    fn emit(&mut self, id: u64) {
        self.notify(id);
        self.dirty = true;
    }

    fn set_state(&mut self, id: u64, state: JobState) {
        if let Some(j) = self.job_mut(id) {
            j.state = state;
            j.speed = 0.0;
            if state != JobState::Failed {
                j.error = None;
            }
        }
        self.emit(id);
    }

    fn handle(&mut self, c: Command) {
        match c {
            Command::Enqueue(new) => {
                for n in new {
                    let id = self.next_id;
                    self.next_id += 1;
                    self.jobs.push(Job {
                        id,
                        item_id: n.item_id,
                        title: n.title,
                        url: n.url,
                        target: n.target,
                        expected_size: n.expected_size,
                        state: JobState::Queued,
                        downloaded: 0,
                        total: n.expected_size,
                        error: None,
                        speed: 0.0,
                        retry: 0,
                    });
                    self.emit(id);
                }
            }
            Command::Pause(id) => {
                let state = self.job_mut(id).map(|j| j.state);
                if let Some(r) = self.running.get_mut(&id) {
                    r.stop = Some(Stop::Pause);
                    r.token.cancel();
                    self.set_state(id, JobState::Paused);
                } else if state == Some(JobState::Queued) {
                    self.set_state(id, JobState::Paused);
                }
            }
            Command::Resume(id) => {
                if self
                    .job_mut(id)
                    .is_some_and(|j| j.state == JobState::Paused)
                    && !self.running.contains_key(&id)
                {
                    self.set_state(id, JobState::Queued);
                }
            }
            Command::Retry(id) => {
                let ok = self.job_mut(id).is_some_and(|j| {
                    matches!(
                        j.state,
                        JobState::Failed | JobState::Canceled | JobState::Paused
                    )
                });
                if ok && !self.running.contains_key(&id) {
                    self.set_state(id, JobState::Queued);
                }
            }
            Command::Cancel(id) => {
                if let Some(r) = self.running.get_mut(&id) {
                    // The .part file is removed once the worker has closed it.
                    r.stop = Some(Stop::Cancel);
                    r.token.cancel();
                    self.set_state(id, JobState::Canceled);
                } else if let Some(j) = self.jobs.iter().find(|j| j.id == id)
                    && j.state != JobState::Done
                {
                    // Not running (queued, paused, failed or canceled): remove it from the list.
                    let _ = std::fs::remove_file(j.part_path());
                    self.jobs.retain(|j| j.id != id);
                    let _ = self.events.send(Event::Removed(id));
                    self.dirty = true;
                }
            }
            Command::SetMaxParallel(n) => {
                self.max_parallel = n.clamp(1, MAX_PARALLEL_LIMIT);
                let _ = self.events.send(Event::MaxParallel(self.max_parallel));
            }
            Command::ClearFinished => {
                let gone: Vec<u64> = self
                    .jobs
                    .iter()
                    .filter(|j| matches!(j.state, JobState::Done | JobState::Canceled))
                    // A canceled job stays until its worker has stopped: its .part
                    // file is removed at that time.
                    .filter(|j| !self.running.contains_key(&j.id))
                    .map(|j| j.id)
                    .collect();
                self.jobs.retain(|j| !gone.contains(&j.id));
                for id in gone {
                    let _ = self.events.send(Event::Removed(id));
                }
                self.dirty = true;
            }
            Command::Shutdown(_) => unreachable!("handled in run()"),
        }
    }

    fn on_worker(&mut self, msg: WorkerMsg) {
        match msg {
            WorkerMsg::Progress {
                id,
                downloaded,
                total,
                speed,
                retry,
            } => {
                if let Some(j) = self.job_mut(id)
                    && j.state == JobState::Running
                {
                    j.downloaded = downloaded;
                    j.total = total.or(j.total);
                    j.speed = speed;
                    j.retry = retry;
                    // Progress alone does not save the queue: the .part file has it.
                    self.notify(id);
                }
            }
            WorkerMsg::Finished { id, result } => {
                let stop = self.running.remove(&id).and_then(|r| r.stop);
                let Some(j) = self.job_mut(id) else { return };
                j.speed = 0.0;
                match result {
                    Ok(n) => {
                        j.state = JobState::Done;
                        j.downloaded = n;
                        j.total = Some(n);
                        j.error = None;
                    }
                    Err(WorkerError::Canceled) => match stop {
                        Some(Stop::Pause) => j.state = JobState::Paused,
                        Some(Stop::Cancel) => {
                            let _ = std::fs::remove_file(j.part_path());
                            j.downloaded = 0;
                            j.state = JobState::Canceled;
                        }
                        Some(Stop::Requeue) | None => j.state = JobState::Queued,
                    },
                    Err(WorkerError::Fatal(e)) => {
                        tracing::error!(id, "download failed: {e}");
                        j.state = JobState::Failed;
                        j.error = Some(e);
                    }
                }
                self.emit(id);
            }
        }
    }

    fn has_startable(&self) -> bool {
        self.running.len() < self.max_parallel
            && self
                .jobs
                .iter()
                .any(|j| j.state == JobState::Queued && !self.running.contains_key(&j.id))
    }

    fn waiting_for_delay(&self) -> bool {
        Instant::now() < self.next_start && self.has_startable()
    }

    /// Start queued jobs until the parallel limit is reached.
    fn schedule(&mut self) {
        while self.running.len() < self.max_parallel {
            if Instant::now() < self.next_start {
                return;
            }
            let Some(idx) = self
                .jobs
                .iter()
                .position(|j| j.state == JobState::Queued && !self.running.contains_key(&j.id))
            else {
                return;
            };
            let j = &mut self.jobs[idx];
            let id = j.id;

            // Skip files that are already complete on disk.
            if let Ok(meta) = std::fs::metadata(&j.target)
                && j.expected_size.is_none_or(|e| e == meta.len())
            {
                j.state = JobState::Done;
                j.downloaded = meta.len();
                j.total = Some(meta.len());
                self.emit(id);
                continue;
            }

            j.state = JobState::Running;
            j.error = None;
            j.retry = 0;
            let spec = Spec {
                id,
                url: j.url.clone(),
                target: j.target.clone(),
                expected_size: j.expected_size,
            };
            let token = CancellationToken::new();
            self.running.insert(
                id,
                Running {
                    token: token.clone(),
                    stop: None,
                },
            );
            let ctx = self.ctx.clone();
            let tx = self.worker_tx.clone();
            tokio::spawn(async move { worker::run(&ctx, spec, token, tx).await });
            if let Some((min, max)) = self.start_delay {
                let ms = fastrand::u64(min.as_millis() as u64..=max.as_millis() as u64);
                self.next_start = Instant::now() + Duration::from_millis(ms);
                tracing::info!("next download can start in {ms} ms");
            }
            self.emit(id);
        }
    }
}
