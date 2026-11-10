//! Save and load the download queue, so downloads resume after a restart.

use std::path::Path;

use anyhow::{Context, Result};

use super::job::{Job, JobState};

pub fn save(path: &Path, jobs: &[Job]) -> Result<()> {
    let keep: Vec<&Job> = jobs
        .iter()
        .filter(|j| j.state != JobState::Canceled)
        .collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&keep)?)
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

/// Load the saved queue. Jobs that were running become queued again.
pub fn load(path: &Path) -> Result<Vec<Job>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut jobs: Vec<Job> = serde_json::from_slice(&data)
        .with_context(|| format!("bad queue file {}", path.display()))?;
    for j in &mut jobs {
        if j.state == JobState::Running {
            j.state = JobState::Queued;
        }
    }
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_requeues_running_and_drops_canceled() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("q.json");
        let mk = |id, state| Job {
            id,
            item_id: format!("i{id}"),
            title: "t".into(),
            url: "http://x".into(),
            target: "/tmp/a.mkv".into(),
            expected_size: Some(5),
            state,
            downloaded: 2,
            total: Some(5),
            error: None,
            speed: 3.0,
            retry: 0,
        };
        save(
            &f,
            &[
                mk(1, JobState::Running),
                mk(2, JobState::Canceled),
                mk(3, JobState::Paused),
            ],
        )
        .unwrap();
        let jobs = load(&f).unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].state, JobState::Queued);
        assert_eq!(jobs[0].speed, 0.0);
        assert_eq!(jobs[1].state, JobState::Paused);
    }
}
