//! token-bucket bandwidth limiter; rate 0 = unlimited
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Limiter {
    rate_bps: AtomicU64,
    inner: Mutex<Bucket>,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Limiter {
    pub fn new(rate_bps: u64) -> Self {
        Self {
            rate_bps: AtomicU64::new(rate_bps),
            inner: Mutex::new(Bucket { tokens: 0.0, last: Instant::now() }),
        }
    }

    pub fn set_rate(&self, bps: u64) {
        self.rate_bps.store(bps, Ordering::Relaxed);
    }

    pub fn rate(&self) -> u64 {
        self.rate_bps.load(Ordering::Relaxed)
    }

    /// await until `want` tokens are available; grants exactly what's pooled
    pub async fn acquire(&self, want: usize) -> usize {
        let rate = self.rate_bps.load(Ordering::Relaxed);
        if rate == 0 || want == 0 {
            return want;
        }
        let cap = (rate as f64 * 1.5).max(64.0 * 1024.0); // burst headroom
        loop {
            let wait = {
                let mut b = self.inner.lock().unwrap();
                let now = Instant::now();
                let dt = now.duration_since(b.last).as_secs_f64();
                b.last = now;
                b.tokens = (b.tokens + dt * rate as f64).min(cap);
                if b.tokens >= want as f64 {
                    b.tokens -= want as f64;
                    Duration::ZERO
                } else {
                    let deficit = (want as f64 - b.tokens) / rate as f64;
                    Duration::from_secs_f64(deficit.min(1.0).max(0.001))
                }
            };
            if wait.is_zero() {
                return want;
            }
            tokio::time::sleep(wait).await;
        }
    }
}

