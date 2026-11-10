//! Download one file, with resume (HTTP Range), retries and cancellation.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::StreamExt;
use reqwest::{Client, StatusCode, header};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use super::job::part_path;

const PROGRESS_EVERY: Duration = Duration::from_millis(250);

pub struct WorkerCtx {
    pub client: Client,
    pub auth_header: String,
    pub retry_base: Duration,
    pub max_retries: u32,
    /// Shared speed limit for all downloads.
    pub limiter: Option<std::sync::Arc<super::limiter::RateLimiter>>,
}

pub struct Spec {
    pub id: u64,
    pub url: String,
    pub target: PathBuf,
    pub expected_size: Option<u64>,
}

#[derive(Debug)]
pub enum WorkerMsg {
    Progress {
        id: u64,
        downloaded: u64,
        total: Option<u64>,
        speed: f64,
        retry: u32,
    },
    Finished {
        id: u64,
        result: Result<u64, WorkerError>,
    },
}

#[derive(Debug, PartialEq)]
pub enum WorkerError {
    Canceled,
    Fatal(String),
}

enum AttemptError {
    Canceled,
    Retry(String),
    Fatal(String),
}

pub async fn run(
    ctx: &WorkerCtx,
    spec: Spec,
    token: CancellationToken,
    tx: UnboundedSender<WorkerMsg>,
) {
    let result = run_inner(ctx, &spec, &token, &tx).await;
    let _ = tx.send(WorkerMsg::Finished {
        id: spec.id,
        result,
    });
}

async fn run_inner(
    ctx: &WorkerCtx,
    spec: &Spec,
    token: &CancellationToken,
    tx: &UnboundedSender<WorkerMsg>,
) -> Result<u64, WorkerError> {
    let mut retry = 0u32;
    loop {
        let before = file_len(&part_path(&spec.target)).await;
        match attempt(ctx, spec, token, tx, retry).await {
            Ok(n) => return Ok(n),
            Err(AttemptError::Canceled) => return Err(WorkerError::Canceled),
            Err(AttemptError::Fatal(e)) => return Err(WorkerError::Fatal(e)),
            Err(AttemptError::Retry(e)) => {
                // Progress was made: the connection is flaky, not dead. Start counting again.
                if file_len(&part_path(&spec.target)).await > before {
                    retry = 0;
                }
                if retry >= ctx.max_retries {
                    return Err(WorkerError::Fatal(format!(
                        "{e} (gave up after {retry} retries)"
                    )));
                }
                let wait = ctx.retry_base * 2u32.pow(retry);
                retry += 1;
                tracing::warn!(
                    id = spec.id,
                    "download error: {e}; retry {retry} in {wait:?}"
                );
                tokio::select! {
                    _ = token.cancelled() => return Err(WorkerError::Canceled),
                    _ = tokio::time::sleep(wait) => {}
                }
            }
        }
    }
}

async fn file_len(p: &Path) -> u64 {
    tokio::fs::metadata(p).await.map(|m| m.len()).unwrap_or(0)
}

async fn finalize(part: &Path, target: &Path, size: u64) -> Result<u64, AttemptError> {
    tokio::fs::rename(part, target)
        .await
        .map_err(|e| AttemptError::Fatal(format!("cannot rename to {}: {e}", target.display())))?;
    Ok(size)
}

/// Parse the start and the total from `Content-Range: bytes 100-199/1000`.
fn parse_content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let rest = v.strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let start = range.split_once('-')?.0.trim().parse().ok()?;
    Some((start, total.trim().parse().ok()))
}

async fn attempt(
    ctx: &WorkerCtx,
    spec: &Spec,
    token: &CancellationToken,
    tx: &UnboundedSender<WorkerMsg>,
    retry: u32,
) -> Result<u64, AttemptError> {
    let part = part_path(&spec.target);
    if let Some(dir) = spec.target.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| AttemptError::Fatal(format!("cannot create {}: {e}", dir.display())))?;
    }
    let mut have = file_len(&part).await;

    if let Some(exp) = spec.expected_size {
        if have == exp && exp > 0 {
            return finalize(&part, &spec.target, have).await;
        }
        if have > exp {
            // The partial file is bigger than the real one: it is not ours. Start again.
            let _ = tokio::fs::remove_file(&part).await;
            have = 0;
        }
    }

    let mut req = ctx
        .client
        .get(&spec.url)
        .header(header::AUTHORIZATION, &ctx.auth_header);
    if have > 0 {
        req = req.header(header::RANGE, format!("bytes={have}-"));
    }
    let res = tokio::select! {
        _ = token.cancelled() => return Err(AttemptError::Canceled),
        r = req.send() => r.map_err(|e| AttemptError::Retry(format!("request failed: {e}")))?,
    };

    let status = res.status();
    let mut total = spec.expected_size;
    let append = match status {
        StatusCode::PARTIAL_CONTENT => {
            let cr = res
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range);
            match cr {
                Some((start, t)) if start == have => {
                    total = total.or(t);
                    true
                }
                _ => {
                    let _ = tokio::fs::remove_file(&part).await;
                    return Err(AttemptError::Retry("server sent a wrong byte range".into()));
                }
            }
        }
        StatusCode::OK => {
            // The server ignored Range (or there was none): write from the start.
            have = 0;
            total = total.or(res.content_length());
            false
        }
        StatusCode::RANGE_NOT_SATISFIABLE => {
            if spec.expected_size.is_none_or(|e| e == have) {
                return finalize(&part, &spec.target, have).await;
            }
            let _ = tokio::fs::remove_file(&part).await;
            return Err(AttemptError::Retry("range not satisfiable".into()));
        }
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err(AttemptError::Fatal(format!(
                "HTTP {status}: access denied (the user may lack the download permission)"
            )));
        }
        s if s.is_server_error() || s == StatusCode::TOO_MANY_REQUESTS => {
            return Err(AttemptError::Retry(format!("HTTP {s}")));
        }
        s => return Err(AttemptError::Fatal(format!("HTTP {s}"))),
    };

    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&part)
        .await
        .map_err(|e| AttemptError::Fatal(format!("cannot open {}: {e}", part.display())))?;
    let mut out = tokio::io::BufWriter::with_capacity(256 * 1024, file);
    let write_err = |e: std::io::Error| AttemptError::Fatal(format!("write error: {e}"));

    let mut stream = res.bytes_stream();
    let mut done = have;
    let mut last_report = Instant::now();
    let mut bytes_since = 0u64;
    let mut speed = 0.0f64;
    let report = |done: u64, speed: f64| {
        let _ = tx.send(WorkerMsg::Progress {
            id: spec.id,
            downloaded: done,
            total,
            speed,
            retry,
        });
    };
    report(done, 0.0);

    loop {
        let chunk = tokio::select! {
            _ = token.cancelled() => {
                out.flush().await.map_err(write_err)?;
                return Err(AttemptError::Canceled);
            }
            c = stream.next() => c,
        };
        match chunk {
            Some(Ok(b)) => {
                out.write_all(&b).await.map_err(write_err)?;
                if let Some(l) = &ctx.limiter {
                    tokio::select! {
                        _ = token.cancelled() => {
                            out.flush().await.map_err(write_err)?;
                            return Err(AttemptError::Canceled);
                        }
                        _ = l.acquire(b.len()) => {}
                    }
                }
                done += b.len() as u64;
                bytes_since += b.len() as u64;
                let dt = last_report.elapsed();
                if dt >= PROGRESS_EVERY {
                    let inst = bytes_since as f64 / dt.as_secs_f64();
                    speed = if speed == 0.0 {
                        inst
                    } else {
                        0.3 * inst + 0.7 * speed
                    };
                    report(done, speed);
                    last_report = Instant::now();
                    bytes_since = 0;
                }
            }
            Some(Err(e)) => {
                out.flush().await.map_err(write_err)?;
                return Err(AttemptError::Retry(format!("connection lost: {e}")));
            }
            None => break,
        }
    }
    out.flush().await.map_err(write_err)?;
    drop(out);
    report(done, 0.0);

    match total {
        Some(t) if done < t => Err(AttemptError::Retry(format!(
            "incomplete: {done} of {t} bytes"
        ))),
        Some(t) if done > t => {
            let _ = tokio::fs::remove_file(&part).await;
            Err(AttemptError::Fatal(format!(
                "got {done} bytes, expected {t}"
            )))
        }
        _ => finalize(&part, &spec.target, done).await,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_content_range;

    #[test]
    fn content_range() {
        assert_eq!(
            parse_content_range("bytes 100-199/1000"),
            Some((100, Some(1000)))
        );
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, None)));
        assert_eq!(parse_content_range("items 1-2/3"), None);
    }
}
