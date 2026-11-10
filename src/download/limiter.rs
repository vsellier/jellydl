//! A shared speed limit for all downloads (token bucket).

use std::time::{Duration, Instant};

use tokio::sync::Mutex;

pub struct RateLimiter {
    rate: f64,
    burst: f64,
    state: Mutex<(f64, Instant)>,
}

impl RateLimiter {
    /// `bytes_per_sec` is the total for all callers together.
    pub fn new(bytes_per_sec: u64) -> Self {
        let rate = bytes_per_sec.max(1) as f64;
        Self {
            rate,
            // Small burst: the speed stays close to the limit even over short times.
            burst: (rate * 0.1).max(16.0 * 1024.0),
            state: Mutex::new((0.0, Instant::now())),
        }
    }

    pub fn bytes_per_sec(&self) -> u64 {
        self.rate as u64
    }

    /// Account for `n` bytes, and wait if they go over the limit.
    pub async fn acquire(&self, n: usize) {
        let wait = {
            let mut s = self.state.lock().await;
            let now = Instant::now();
            let (tokens, last) = &mut *s;
            *tokens =
                (*tokens + now.duration_since(*last).as_secs_f64() * self.rate).min(self.burst);
            *last = now;
            // The balance can go negative: the next callers then wait for the debt too.
            *tokens -= n as f64;
            if *tokens < 0.0 {
                Duration::from_secs_f64(-*tokens / self.rate)
            } else {
                Duration::ZERO
            }
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn limits_shared_rate() {
        let l = std::sync::Arc::new(RateLimiter::new(1024 * 1024));
        let start = Instant::now();
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let l = l.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..32 {
                    l.acquire(4096).await; // 4 tasks x 128 KB = 512 KB
                }
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let secs = start.elapsed().as_secs_f64();
        // 512 KB at 1 MB/s = 0.5 s, minus the ~100 KB burst.
        assert!(secs > 0.35, "too fast: {secs}");
        assert!(secs < 1.0, "too slow: {secs}");
    }
}
