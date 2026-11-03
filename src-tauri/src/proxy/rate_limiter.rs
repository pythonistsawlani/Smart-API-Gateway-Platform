//! In-memory token-bucket rate limiter for the proxy.
//!
//! - One bucket per dimension: `ip:<addr>`, `key:<access key>`, `model:<name>`.
//! - `check()` is two-pass: it validates every dimension before consuming any
//!   tokens, so a rejected request never partially consumes budget.
//! - Idle buckets are pruned to keep memory bounded (design target: 10k active
//!   buckets, prune every 5 minutes once the map grows past 2k buckets).
//!
//! The config is read from `AppSettings` at check time, so settings changes
//! take effect without a restart. The structure is deliberately swappable
//! later for a Redis-backed implementation without touching the handlers.

use crate::database::AppSettings;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Max buckets kept in memory before pruning is forced.
pub const MAX_BUCKETS: usize = 10_000;
/// Only prune once the map has grown beyond this size.
pub const IDLE_PRUNE_THRESHOLD: usize = 2_000;
/// Buckets untouched (fully refilled) for this long are removed.
pub const IDLE_PRUNE_AFTER: Duration = Duration::from_secs(300);

/// Outcome of a rate-limit check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitExceeded {
    /// Seconds the client should wait before retrying (for `Retry-After`).
    pub retry_after_secs: u64,
}

/// Rate limiting configuration, derived from settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub per_ip_rpm: u64,
    pub per_key_rpm: u64,
    pub per_model_rpm: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            per_ip_rpm: 120,
            per_key_rpm: 240,
            per_model_rpm: 600,
        }
    }
}

impl RateLimitConfig {
    pub fn from_settings(settings: &AppSettings) -> Self {
        Self {
            enabled: settings.rate_limit_enabled,
            per_ip_rpm: settings.rate_limit_rpm_per_ip.max(1) as u64,
            per_key_rpm: settings.rate_limit_rpm_per_key.max(1) as u64,
            per_model_rpm: settings.rate_limit_rpm_per_model.max(1) as u64,
        }
    }
}

#[derive(Debug)]
struct Bucket {
    capacity: f64,
    tokens: f64,
    last_refill: Instant,
    last_used: Instant,
}

impl Bucket {
    fn new(capacity: f64) -> Self {
        Self {
            capacity,
            tokens: capacity,
            last_refill: Instant::now(),
            last_used: Instant::now(),
        }
    }

    fn refill(&mut self, rate_per_minute: f64) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * rate_per_minute / 60.0).min(self.capacity);
            self.last_refill = now;
        }
    }

    fn wait_after_deficit(&self, rate_per_minute: f64, deficit: f64) -> Duration {
        let wait_secs = (deficit / (rate_per_minute / 60.0)).ceil() as u64;
        Duration::from_secs(wait_secs.max(1))
    }

    /// Validates without consuming: refills and reports the wait if the
    /// bucket cannot satisfy `tokens`.
    fn peek(&mut self, rate_per_minute: f64, tokens: f64) -> Option<Duration> {
        self.refill(rate_per_minute);
        if self.tokens >= tokens {
            None
        } else {
            Some(self.wait_after_deficit(rate_per_minute, tokens - self.tokens))
        }
    }

    /// Consumes `tokens` (default 1). Returns `Some(wait)` when rejected.
    fn try_take(&mut self, rate_per_minute: f64, tokens: f64) -> Option<Duration> {
        self.last_used = Instant::now();
        match self.peek(rate_per_minute, tokens) {
            None => {
                self.tokens -= tokens;
                None
            }
            Some(wait) => Some(wait),
        }
    }

    fn is_idle(&self) -> bool {
        self.last_used.elapsed() >= IDLE_PRUNE_AFTER
    }
}

#[derive(Debug)]
struct Inner {
    buckets: HashMap<String, Bucket>,
    last_prune: Instant,
}

impl Inner {
    /// Prune idle, fully-refilled buckets.
    fn prune(&mut self) {
        self.buckets.retain(|_, bucket| !bucket.is_idle());
        self.last_prune = Instant::now();
    }
}

/// Thread-safe token-bucket limiter. Cheap to clone (wraps an `Arc`-style
/// interior), so it can be shared through `ProxyState`.
#[derive(Debug)]
pub struct RateLimiter {
    inner: Mutex<Inner>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                buckets: HashMap::new(),
                last_prune: Instant::now(),
            }),
        }
    }

    /// Check the request against all configured dimensions.
    ///
    /// Two-pass: every dimension is validated first; only when all of them
    /// pass does any bucket consume a token. Returns the longest wait when a
    /// dimension is exhausted.
    pub fn check(
        &self,
        config: &RateLimitConfig,
        ip: Option<&str>,
        access_key: Option<&str>,
        model: Option<&str>,
    ) -> Option<RateLimitExceeded> {
        if !config.enabled {
            return None;
        }

        let mut inner = self.inner.lock().unwrap();
        if inner.buckets.len() >= IDLE_PRUNE_THRESHOLD
            && inner.last_prune.elapsed() >= IDLE_PRUNE_AFTER
        {
            inner.prune();
        }

        let mut dimensions: Vec<(String, u64)> = Vec::with_capacity(3);
        if let Some(ip) = ip {
            if !ip.is_empty() {
                dimensions.push((format!("ip:{ip}"), config.per_ip_rpm));
            }
        }
        if let Some(key) = access_key {
            if !key.is_empty() {
                dimensions.push((format!("key:{key}"), config.per_key_rpm));
            }
        }
        if let Some(model) = model {
            if !model.is_empty() && model != "auto" {
                dimensions.push((format!("model:{model}"), config.per_model_rpm));
            }
        }
        if dimensions.is_empty() {
            return None;
        }

        // Pass 1: validate without consuming.
        let mut waits: Vec<u64> = Vec::with_capacity(dimensions.len());
        for (key, rpm) in &dimensions {
            let bucket = inner
                .buckets
                .entry(key.clone())
                .or_insert_with(|| Bucket::new(*rpm as f64));
            if let Some(wait) = bucket.peek(*rpm as f64, 1.0) {
                waits.push(wait.as_secs());
            }
        }

        if !waits.is_empty() {
            // Rejected: nothing was consumed beyond refills.
            let retry_after_secs = waits.into_iter().max().unwrap_or(1);
            if inner.buckets.len() >= MAX_BUCKETS {
                inner.prune();
            }
            return Some(RateLimitExceeded { retry_after_secs });
        }

        // Pass 2: consume one token from every dimension.
        for (key, rpm) in &dimensions {
            if let Some(bucket) = inner.buckets.get_mut(key) {
                let _ = bucket.try_take(*rpm as f64, 1.0);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(per_ip_rpm: u64) -> RateLimitConfig {
        RateLimitConfig {
            enabled: true,
            per_ip_rpm,
            per_key_rpm: 100,
            per_model_rpm: 200,
        }
    }

    #[test]
    fn allows_requests_under_limit() {
        let limiter = RateLimiter::new();
        let cfg = config(10);
        for _ in 0..10 {
            assert_eq!(limiter.check(&cfg, Some("1.2.3.4"), None, None), None);
        }
    }

    #[test]
    fn rejects_requests_over_limit() {
        let limiter = RateLimiter::new();
        let cfg = config(2);
        assert_eq!(limiter.check(&cfg, Some("1.2.3.4"), None, None), None);
        assert_eq!(limiter.check(&cfg, Some("1.2.3.4"), None, None), None);
        let exceeded = limiter
            .check(&cfg, Some("1.2.3.4"), None, None)
            .expect("should be rate limited");
        assert!(exceeded.retry_after_secs >= 1);
    }

    #[test]
    fn rejects_before_consuming_any_dimension() {
        // Key bucket exhausted (1/min) must also prevent consumption of the
        // IP bucket, so a follow-up request from a fresh key still succeeds.
        let limiter = RateLimiter::new();
        let cfg = RateLimitConfig {
            enabled: true,
            per_ip_rpm: 2,
            per_key_rpm: 1,
            per_model_rpm: 100,
        };
        assert_eq!(limiter.check(&cfg, Some("9.9.9.9"), Some("key-a"), None), None);
        assert!(
            limiter
                .check(&cfg, Some("9.9.9.9"), Some("key-a"), None)
                .is_some()
        );
        // Same IP, different key: IP bucket must still hold 2 tokens.
        assert_eq!(limiter.check(&cfg, Some("9.9.9.9"), Some("key-b"), None), None);
    }

    #[test]
    fn buckets_are_independent_per_dimension() {
        let limiter = RateLimiter::new();
        let cfg = config(1);
        assert_eq!(limiter.check(&cfg, Some("1.1.1.1"), None, None), None);
        assert!(limiter.check(&cfg, Some("1.1.1.1"), None, None).is_some());
        // Different IP is unaffected.
        assert_eq!(limiter.check(&cfg, Some("2.2.2.2"), None, None), None);
    }

    #[test]
    fn disabled_config_never_limits() {
        let limiter = RateLimiter::new();
        let cfg = RateLimitConfig {
            enabled: false,
            ..config(1)
        };
        for _ in 0..100 {
            assert_eq!(limiter.check(&cfg, Some("1.2.3.4"), None, None), None);
        }
    }

    #[test]
    fn missing_identity_is_not_limited() {
        let limiter = RateLimiter::new();
        let cfg = config(1);
        for _ in 0..100 {
            assert_eq!(limiter.check(&cfg, None, None, None), None);
        }
    }

    #[test]
    fn prune_removes_idle_buckets() {
        let limiter = RateLimiter::new();
        let cfg = config(5);
        for i in 0..10 {
            assert_eq!(
                limiter.check(&cfg, Some(&format!("10.0.0.{i}")), None, None),
                None
            );
        }
        assert_eq!(limiter.inner.lock().unwrap().buckets.len(), 10);

        // Simulate expiry: age the buckets so they count as idle.
        for bucket in limiter.inner.lock().unwrap().buckets.values_mut() {
            bucket.last_used = Instant::now() - IDLE_PRUNE_AFTER - Duration::from_secs(1);
        }
        limiter.inner.lock().unwrap().prune();
        assert_eq!(limiter.inner.lock().unwrap().buckets.len(), 0);
    }
}
