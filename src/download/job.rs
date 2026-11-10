use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Canceled,
}

impl JobState {
    /// Queued or running: the job still needs work without user action.
    pub fn is_active(self) -> bool {
        matches!(self, JobState::Queued | JobState::Running)
    }

    pub fn label(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Paused => "paused",
            JobState::Done => "done",
            JobState::Failed => "failed",
            JobState::Canceled => "canceled",
        }
    }
}

/// A request to download one item.
#[derive(Debug, Clone)]
pub struct NewJob {
    pub item_id: String,
    pub title: String,
    pub url: String,
    pub target: PathBuf,
    pub expected_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub item_id: String,
    pub title: String,
    pub url: String,
    pub target: PathBuf,
    pub expected_size: Option<u64>,
    pub state: JobState,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
    /// Bytes per second (smoothed). Not saved.
    #[serde(skip)]
    pub speed: f64,
    /// Retry number of the current attempt (0 = first try). Not saved.
    #[serde(skip)]
    pub retry: u32,
}

impl Job {
    pub fn part_path(&self) -> PathBuf {
        part_path(&self.target)
    }

    pub fn eta_secs(&self) -> Option<u64> {
        let total = self.total?;
        if self.state != JobState::Running || self.speed < 1.0 {
            return None;
        }
        Some((total.saturating_sub(self.downloaded) as f64 / self.speed) as u64)
    }

    pub fn fraction(&self) -> Option<f64> {
        match self.total {
            Some(t) if t > 0 => Some((self.downloaded as f64 / t as f64).clamp(0.0, 1.0)),
            _ => None,
        }
    }
}

/// Sizes of the downloads that are not finished (queued, running or paused).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    /// Number of unfinished downloads.
    pub count: usize,
    pub downloaded: u64,
    /// Sum of the known sizes.
    pub total: u64,
    /// Downloads with an unknown size. They are not in `total`.
    pub unknown: usize,
    /// Bytes left in queued and running downloads. Paused ones do not move.
    pub remaining: u64,
}

impl Totals {
    pub fn of<'a>(jobs: impl IntoIterator<Item = &'a Job>) -> Self {
        let mut t = Totals::default();
        for j in jobs {
            if !matches!(
                j.state,
                JobState::Queued | JobState::Running | JobState::Paused
            ) {
                continue;
            }
            t.count += 1;
            match j.total.or(j.expected_size) {
                Some(size) => {
                    let done = j.downloaded.min(size);
                    t.downloaded += done;
                    t.total += size;
                    if j.state.is_active() {
                        t.remaining += size - done;
                    }
                }
                None => {
                    t.unknown += 1;
                    t.downloaded += j.downloaded;
                }
            }
        }
        t
    }

    pub fn fraction(&self) -> Option<f64> {
        (self.total > 0).then(|| self.downloaded as f64 / self.total as f64)
    }

    /// Seconds until the queued and running downloads are done, at `speed` bytes per second.
    pub fn eta_secs(&self, speed: f64) -> Option<u64> {
        (self.remaining > 0 && speed >= 1.0).then(|| (self.remaining as f64 / speed) as u64)
    }
}

pub fn part_path(target: &std::path::Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(state: JobState, downloaded: u64, total: Option<u64>) -> Job {
        Job {
            id: 1,
            item_id: "i".into(),
            title: "t".into(),
            url: "u".into(),
            target: PathBuf::from("/x"),
            expected_size: total,
            state,
            downloaded,
            total,
            error: None,
            speed: 0.0,
            retry: 0,
        }
    }

    #[test]
    fn totals_of_unfinished_downloads() {
        let jobs = [
            job(JobState::Running, 300, Some(1000)),
            job(JobState::Queued, 0, Some(500)),
            job(JobState::Paused, 100, Some(200)),
            job(JobState::Queued, 50, None),
            job(JobState::Done, 999, Some(999)),
            job(JobState::Failed, 10, Some(100)),
        ];
        let t = Totals::of(&jobs);
        assert_eq!(
            t,
            Totals {
                count: 4,
                downloaded: 450,
                total: 1700,
                unknown: 1,
                // The paused job is not in the time estimate.
                remaining: 700 + 500,
            }
        );
        assert_eq!(t.eta_secs(100.0), Some(12));
        assert_eq!(t.eta_secs(0.0), None);
        assert_eq!(Totals::of(&[]).fraction(), None);
    }
}
