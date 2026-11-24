use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

// Constants

const BURST: Duration = Duration::from_millis(250);
const SMALLEST_CHUNK: u64 = 4 * 1024;
const CHUNKS_PER_SECOND: u64 = 8;

// Types

#[derive(Debug)]
pub struct RateLimiter {
    bucket: Mutex<Bucket>,
    changed: Notify,
}

#[derive(Debug)]
struct Bucket {
    rate: Option<u64>,
    tokens: f64,
    updated: Instant,
}

// Bucket

impl Bucket {
    fn refill(&mut self, now: Instant) {
        if let Some(rate) = self.rate {
            let elapsed = now.saturating_duration_since(self.updated).as_secs_f64();
            let capacity = rate as f64 * BURST.as_secs_f64();
            self.tokens = (self.tokens + elapsed * rate as f64).min(capacity);
        }
        self.updated = now;
    }
}

// Limiter

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(None)
    }
}

impl RateLimiter {
    pub fn new(bytes_per_second: Option<u64>) -> Self {
        Self {
            bucket: Mutex::new(Bucket {
                rate: bytes_per_second.filter(|rate| *rate > 0),
                tokens: 0.0,
                updated: Instant::now(),
            }),
            changed: Notify::new(),
        }
    }

    pub fn rate(&self) -> Option<u64> {
        self.bucket().rate
    }

    pub fn set_rate(&self, bytes_per_second: Option<u64>) {
        let rate = bytes_per_second.filter(|rate| *rate > 0);
        {
            let mut bucket = self.bucket();
            if bucket.rate == rate {
                return;
            }
            let now = Instant::now();
            bucket.refill(now);
            bucket.tokens = match (bucket.rate, rate) {
                (Some(_), Some(new)) => bucket.tokens.min(new as f64 * BURST.as_secs_f64()),
                _ => 0.0,
            };
            bucket.rate = rate;
        }
        self.changed.notify_waiters();
    }

    pub fn chunk_size(&self, largest: usize) -> usize {
        match self.rate() {
            None => largest,
            Some(rate) => {
                let chunk = (rate / CHUNKS_PER_SECOND).max(SMALLEST_CHUNK);
                usize::try_from(chunk).map_or(largest, |chunk| chunk.min(largest))
            }
        }
    }

    pub async fn acquire(&self, amount: u64) {
        let mut taken = false;
        loop {
            let changed = self.changed.notified();
            let wait = {
                let mut bucket = self.bucket();
                let Some(rate) = bucket.rate else {
                    return;
                };
                bucket.refill(Instant::now());
                if !taken {
                    bucket.tokens -= amount as f64;
                    taken = true;
                }
                if bucket.tokens >= 0.0 {
                    return;
                }
                Duration::from_secs_f64(-bucket.tokens / rate as f64)
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = changed => {}
            }
        }
    }

    fn bucket(&self) -> MutexGuard<'_, Bucket> {
        self.bucket.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn elapsed_since(start: Instant) -> f64 {
        Instant::now().duration_since(start).as_secs_f64()
    }

    #[tokio::test(start_paused = true)]
    async fn the_rate_is_held() {
        let limiter = RateLimiter::new(Some(10_000));
        let start = Instant::now();
        for _ in 0..50 {
            limiter.acquire(1_000).await;
        }
        let elapsed = elapsed_since(start);
        assert!((4.9..=5.1).contains(&elapsed), "{elapsed}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_rate_is_shared_between_tasks() {
        let limiter = Arc::new(RateLimiter::new(Some(20_000)));
        let start = Instant::now();
        let tasks: Vec<_> = (0..4)
            .map(|_| {
                let limiter = Arc::clone(&limiter);
                tokio::spawn(async move {
                    for _ in 0..25 {
                        limiter.acquire(1_000).await;
                    }
                })
            })
            .collect();
        for task in tasks {
            task.await.expect("a task failed");
        }
        let elapsed = elapsed_since(start);
        assert!((4.9..=5.1).contains(&elapsed), "{elapsed}");
    }

    #[tokio::test(start_paused = true)]
    async fn rate_changes_apply_to_waiting_tasks() {
        let limiter = Arc::new(RateLimiter::new(Some(1_000)));
        let start = Instant::now();
        let waiting = {
            let limiter = Arc::clone(&limiter);
            tokio::spawn(async move { limiter.acquire(100_000).await })
        };
        tokio::time::sleep(Duration::from_secs(1)).await;
        limiter.set_rate(None);
        waiting.await.expect("the task failed");
        let elapsed = elapsed_since(start);
        assert!(elapsed < 1.1, "{elapsed}");
        assert_eq!(limiter.rate(), None);

        limiter.set_rate(Some(50_000));
        let start = Instant::now();
        limiter.acquire(100_000).await;
        let elapsed = elapsed_since(start);
        assert!((1.9..=2.1).contains(&elapsed), "{elapsed}");
    }

    #[tokio::test(start_paused = true)]
    async fn unlimited_never_waits() {
        let limiter = RateLimiter::new(Some(0));
        assert_eq!(limiter.rate(), None);
        let start = Instant::now();
        limiter.acquire(u64::MAX / 2).await;
        assert!(elapsed_since(start) < 0.001);
    }

    #[test]
    fn chunks_shrink_with_the_rate() {
        assert_eq!(RateLimiter::new(None).chunk_size(131_072), 131_072);
        assert_eq!(RateLimiter::new(Some(1_024)).chunk_size(131_072), 4_096);
        assert_eq!(
            RateLimiter::new(Some(512 * 1024)).chunk_size(131_072),
            65_536
        );
        assert_eq!(
            RateLimiter::new(Some(100 * 1024 * 1024)).chunk_size(131_072),
            131_072
        );
    }
}
