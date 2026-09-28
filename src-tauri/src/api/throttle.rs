//! Holding uploads to a speed.
//!
//! One bucket for the whole app, because a limit is a promise about what the
//! connection gives up in total: two uploads at 5 Mbps each would be 10 Mbps of
//! somebody's upstream, which is not what "limit to 5" means.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How much may go at once after a quiet spell, as a fraction of a second at
/// the current rate. Enough that a limit does not make a transfer stutter
/// between reads; short enough that a burst is not itself a spike.
const BURST: f64 = 0.25;

struct Bucket {
    /// Bytes that may go now. Negative is a debt, paid off by waiting, which
    /// is what keeps a read larger than the burst from waiting forever.
    tokens: f64,
    refilled: Instant,
}

pub struct Throttle {
    /// Bytes per second, or 0 for no limit.
    rate: AtomicU64,
    bucket: tokio::sync::Mutex<Bucket>,
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

impl Throttle {
    pub fn new() -> Self {
        Self {
            rate: AtomicU64::new(0),
            bucket: tokio::sync::Mutex::new(Bucket { tokens: 0.0, refilled: Instant::now() }),
        }
    }

    /// Change the limit. Takes effect on the next read, and a read already
    /// waiting finishes its wait at the old rate — well under a second.
    pub fn set_rate(&self, bytes_per_second: u64) {
        self.rate.store(bytes_per_second, Ordering::Relaxed);
    }

    /// Bytes per second, or 0 for none.
    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Wait until `bytes` may go.
    pub async fn take(&self, bytes: usize) {
        let rate = self.rate();
        if rate == 0 {
            return;
        }
        let rate = rate as f64;
        let debt = {
            let mut bucket = self.bucket.lock().await;
            let now = Instant::now();
            let earned = now.duration_since(bucket.refilled).as_secs_f64() * rate;
            bucket.tokens = (bucket.tokens + earned).min(rate * BURST);
            bucket.refilled = now;
            bucket.tokens -= bytes as f64;
            -bucket.tokens
        };
        if debt > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(debt / rate)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const KIB: usize = 1024;

    async fn send(throttle: &Throttle, total: usize) -> Duration {
        let started = Instant::now();
        for _ in 0..total / (16 * KIB) {
            throttle.take(16 * KIB).await;
        }
        started.elapsed()
    }

    #[tokio::test]
    async fn no_limit_does_not_wait() {
        let throttle = Throttle::new();
        assert!(send(&throttle, 4096 * KIB).await < Duration::from_millis(20));
    }

    #[tokio::test]
    async fn a_limit_holds_to_its_rate() {
        let throttle = Throttle::new();
        throttle.set_rate(512 * KIB as u64);
        // About a second: the bucket starts empty, so there is no burst to
        // spend first.
        let took = send(&throttle, 512 * KIB).await;
        assert!(
            took >= Duration::from_millis(850) && took < Duration::from_millis(1400),
            "took {took:?}"
        );
    }

    /// Two uploads share one limit rather than getting one each.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_uploads_share_the_limit() {
        let throttle = Arc::new(Throttle::new());
        throttle.set_rate(512 * KIB as u64);
        let started = Instant::now();
        let a = tokio::spawn({
            let t = throttle.clone();
            async move { send(&t, 256 * KIB).await }
        });
        let b = tokio::spawn({
            let t = throttle.clone();
            async move { send(&t, 256 * KIB).await }
        });
        a.await.unwrap();
        b.await.unwrap();
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(850), "two halves of the limit, not two limits: {took:?}");
    }

    #[tokio::test]
    async fn lifting_a_limit_takes_effect_straight_away() {
        let throttle = Throttle::new();
        throttle.set_rate(64 * KIB as u64);
        throttle.take(16 * KIB).await;
        throttle.set_rate(0);
        assert!(send(&throttle, 4096 * KIB).await < Duration::from_millis(20));
    }
}
