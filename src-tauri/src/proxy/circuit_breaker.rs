use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::RwLock;
use std::time::Instant;

/// Circuit breaker state for API entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed = 0,
    Open = 1,
    HalfOpen = 2,
}

impl From<u8> for CircuitState {
    fn from(val: u8) -> Self {
        match val {
            1 => CircuitState::Open,
            2 => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        }
    }
}

static JITTER_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Adaptive Circuit Breaker for API entries with exponential backoff and jitter.
/// State is kept in memory only (not persisted).
///
/// State transitions:
/// - Closed → Open: when consecutive failures reach `threshold`.
/// - Open → HalfOpen: when cooldown (base delay * 2^(opens-1) ± jitter) elapses.
/// - HalfOpen → Closed: on successful probe request.
/// - HalfOpen → Open: on failed probe request (increases backoff multiplier).
pub struct CircuitBreaker {
    state: AtomicU8,
    probe_in_flight: AtomicBool,
    consecutive_failures: AtomicU32,
    consecutive_opens: AtomicU32,
    last_opened_at: RwLock<Option<Instant>>,
    last_effective_delay_secs: AtomicU64,
    base_recovery_secs: AtomicU64,
    max_recovery_secs: AtomicU64,
    jitter_ratio: AtomicU64, // f64 stored as u64 bits
}

impl CircuitBreaker {
    /// Create a new circuit breaker with default base recovery seconds.
    pub fn new(recovery_secs: u64) -> Self {
        let max = recovery_secs.saturating_mul(30).max(1800);
        Self::with_config(recovery_secs, max, 0.2)
    }

    /// Create a circuit breaker with custom adaptive backoff configuration.
    pub fn with_config(base_recovery_secs: u64, max_recovery_secs: u64, jitter_ratio: f64) -> Self {
        let max = max_recovery_secs.max(base_recovery_secs);
        let jitter = jitter_ratio.clamp(0.0, 0.5);

        Self {
            state: AtomicU8::new(CircuitState::Closed as u8),
            probe_in_flight: AtomicBool::new(false),
            consecutive_failures: AtomicU32::new(0),
            consecutive_opens: AtomicU32::new(0),
            last_opened_at: RwLock::new(None),
            last_effective_delay_secs: AtomicU64::new(base_recovery_secs),
            base_recovery_secs: AtomicU64::new(base_recovery_secs),
            max_recovery_secs: AtomicU64::new(max),
            jitter_ratio: AtomicU64::new(jitter.to_bits()),
        }
    }

    pub fn get_jitter_ratio(&self) -> f64 {
        f64::from_bits(self.jitter_ratio.load(Ordering::Relaxed))
    }

    pub fn set_jitter_ratio(&self, ratio: f64) {
        let clamped = ratio.clamp(0.0, 0.5);
        self.jitter_ratio.store(clamped.to_bits(), Ordering::Relaxed);
    }

    /// Calculate adaptive backoff delay with exponential growth and bounded jitter.
    pub fn calculate_backoff_delay(&self) -> u64 {
        let opens = self.consecutive_opens.load(Ordering::Relaxed).max(1);
        let shift = (opens - 1).min(10); // cap exponent to prevent overflow
        let multiplier = 1u64.checked_shl(shift).unwrap_or(1024);
        let base = self.base_recovery_secs.load(Ordering::Relaxed);
        let max = self.max_recovery_secs.load(Ordering::Relaxed).max(base);
        let raw_delay = base.saturating_mul(multiplier).min(max);

        let jitter_ratio = self.get_jitter_ratio();
        if jitter_ratio <= 0.0 {
            self.last_effective_delay_secs.store(raw_delay, Ordering::Relaxed);
            return raw_delay;
        }

        let counter = JITTER_COUNTER.fetch_add(1, Ordering::Relaxed);
        let time_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let seed = time_nanos.wrapping_add(counter);
        let pseudo_rand = ((seed.wrapping_mul(6364136223846793005).wrapping_add(1) >> 33) as u32) as f64
            / (u32::MAX as f64);

        let offset_factor = (pseudo_rand * 2.0 - 1.0) * jitter_ratio;
        let delta = (raw_delay as f64 * offset_factor).round() as i64;

        let effective = (raw_delay as i64 + delta)
            .max(base as i64)
            .min(max as i64) as u64;

        self.last_effective_delay_secs.store(effective, Ordering::Relaxed);
        effective
    }

    /// Check whether the circuit breaker allows traffic.
    /// Atomically transitions Open -> HalfOpen when cooldown expires, allowing ONLY ONE probe caller.
    pub fn is_available(&self) -> bool {
        let current_state: CircuitState = self.state.load(Ordering::Acquire).into();

        match current_state {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => {
                // Half-Open state allows exactly one in-flight probe request
                !self.probe_in_flight.load(Ordering::Acquire)
            }
            CircuitState::Open => {
                let last_open = self.last_opened_at.read().ok().and_then(|g| *g);
                if let Some(opened_at) = last_open {
                    let required_cooldown = self.calculate_backoff_delay();
                    if opened_at.elapsed().as_secs() >= required_cooldown {
                        // Attempt atomic transition from Open -> HalfOpen
                        if self
                            .state
                            .compare_exchange(
                                CircuitState::Open as u8,
                                CircuitState::HalfOpen as u8,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_ok()
                        {
                            // Successfully claimed probe
                            self.probe_in_flight.store(true, Ordering::Release);
                            return true;
                        }
                    }
                }
                false
            }
        }
    }

    /// Record a successful request. Resets failure counters, open counters, and transitions to Closed.
    pub fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.consecutive_opens.store(0, Ordering::Relaxed);
        self.probe_in_flight.store(false, Ordering::Release);
        self.state.store(CircuitState::Closed as u8, Ordering::Release);
    }

    /// Record a failed request.
    /// If in HalfOpen state, transitions back to Open and increases exponential backoff.
    /// If in Closed state, increments failures and trips to Open when reaching `threshold`.
    /// Blocked requests while Open do not artificially increment failure counters.
    pub fn record_failure(&self, threshold: u32) {
        let current_state: CircuitState = self.state.load(Ordering::Acquire).into();

        if current_state == CircuitState::HalfOpen || self.probe_in_flight.load(Ordering::Acquire) {
            // Failed recovery probe in HalfOpen state → trip back to Open with increased backoff
            self.consecutive_opens.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut last_opened) = self.last_opened_at.write() {
                *last_opened = Some(Instant::now());
            }
            self.probe_in_flight.store(false, Ordering::Release);
            self.state.store(CircuitState::Open as u8, Ordering::Release);
            return;
        }

        if current_state == CircuitState::Open {
            // Blocked requests while Open do not increment failure counters
            return;
        }

        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= threshold {
            self.consecutive_opens.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut last_opened) = self.last_opened_at.write() {
                *last_opened = Some(Instant::now());
            }
            self.state.store(CircuitState::Open as u8, Ordering::Release);
        }
    }

    /// Get current state of circuit breaker.
    pub fn get_state(&self) -> CircuitState {
        self.state.load(Ordering::Acquire).into()
    }

    /// Get total consecutive failures recorded.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Get total consecutive open cycles recorded.
    pub fn consecutive_opens(&self) -> u32 {
        self.consecutive_opens.load(Ordering::Relaxed)
    }

    /// Get last calculated effective recovery delay in seconds.
    pub fn get_effective_recovery_secs(&self) -> u64 {
        self.last_effective_delay_secs.load(Ordering::Relaxed)
    }

    /// Update base recovery seconds at runtime (e.g., when user alters settings).
    pub fn set_recovery_secs(&self, secs: u64) {
        let base = secs.max(1);
        self.base_recovery_secs.store(base, Ordering::Relaxed);
        let current_max = self.max_recovery_secs.load(Ordering::Relaxed);
        if current_max < base {
            let new_max = base.saturating_mul(30).max(1800);
            self.max_recovery_secs.store(new_max, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_initial_state_is_closed() {
        let cb = CircuitBreaker::new(30);
        assert_eq!(cb.get_state(), CircuitState::Closed);
        assert!(cb.is_available());
        assert_eq!(cb.consecutive_failures(), 0);
        assert_eq!(cb.consecutive_opens(), 0);
    }

    #[test]
    fn test_failure_below_threshold_remains_closed() {
        let cb = CircuitBreaker::new(30);
        for _ in 0..4 {
            cb.record_failure(5);
        }
        assert_eq!(cb.get_state(), CircuitState::Closed);
        assert!(cb.is_available());
        assert_eq!(cb.consecutive_failures(), 4);
    }

    #[test]
    fn test_threshold_opens_circuit() {
        let cb = CircuitBreaker::new(30);
        for _ in 0..5 {
            cb.record_failure(5);
        }
        assert_eq!(cb.get_state(), CircuitState::Open);
        assert_eq!(cb.consecutive_opens(), 1);
    }

    #[test]
    fn test_open_circuit_rejects_requests_before_cooldown() {
        let cb = CircuitBreaker::new(300);
        cb.record_failure(1);
        assert_eq!(cb.get_state(), CircuitState::Open);
        assert!(!cb.is_available());
    }

    #[test]
    fn test_open_circuit_transitions_to_half_open_after_cooldown() {
        let cb = CircuitBreaker::with_config(0, 100, 0.0);
        cb.record_failure(1);
        assert_eq!(cb.get_state(), CircuitState::Open);
        // Instant elapsed >= 0s required cooldown
        assert!(cb.is_available());
        assert_eq!(cb.get_state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_only_one_half_open_probe_is_allowed() {
        let cb = Arc::new(CircuitBreaker::with_config(0, 100, 0.0));
        cb.record_failure(1); // Circuit Open

        // First caller transitions to HalfOpen and claims probe
        let first_claim = cb.is_available();
        assert!(first_claim, "First caller must claim the probe");
        assert_eq!(cb.get_state(), CircuitState::HalfOpen);

        // Concurrent callers while probe is running must be blocked
        let second_claim = cb.is_available();
        assert!(!second_claim, "Second caller must be rejected while probe is in flight");

        // Concurrent threads test
        let cb_clone = cb.clone();
        let handle = thread::spawn(move || cb_clone.is_available());
        assert!(!handle.join().unwrap(), "Concurrent thread must be rejected while probe is in flight");
    }

    #[test]
    fn test_successful_half_open_probe_returns_to_closed() {
        let cb = CircuitBreaker::with_config(0, 100, 0.0);
        cb.record_failure(1);
        assert!(cb.is_available()); // Transitions to HalfOpen
        assert_eq!(cb.get_state(), CircuitState::HalfOpen);

        cb.record_success();
        assert_eq!(cb.get_state(), CircuitState::Closed);
        assert_eq!(cb.consecutive_failures(), 0);
        assert_eq!(cb.consecutive_opens(), 0);
        assert!(cb.is_available());
    }

    #[test]
    fn test_failed_half_open_probe_returns_to_open() {
        let cb = CircuitBreaker::with_config(10, 100, 0.0);
        cb.record_failure(1); // Open cycle 1
        cb.state.store(CircuitState::HalfOpen as u8, Ordering::Relaxed);
        cb.probe_in_flight.store(true, Ordering::Relaxed);

        cb.record_failure(5); // Probe failed
        assert_eq!(cb.get_state(), CircuitState::Open);
        assert_eq!(cb.consecutive_opens(), 2);
        assert_eq!(cb.calculate_backoff_delay(), 20); // 10 * 2^1 = 20s
    }

    #[test]
    fn test_backoff_increases_exponentially() {
        let cb = CircuitBreaker::with_config(10, 1000, 0.0);
        cb.record_failure(1); // 1st open -> 10s
        assert_eq!(cb.calculate_backoff_delay(), 10);

        cb.state.store(CircuitState::HalfOpen as u8, Ordering::Relaxed);
        cb.probe_in_flight.store(true, Ordering::Relaxed);
        cb.record_failure(1); // 2nd open -> 20s
        assert_eq!(cb.calculate_backoff_delay(), 20);

        cb.state.store(CircuitState::HalfOpen as u8, Ordering::Relaxed);
        cb.probe_in_flight.store(true, Ordering::Relaxed);
        cb.record_failure(1); // 3rd open -> 40s
        assert_eq!(cb.calculate_backoff_delay(), 40);

        cb.state.store(CircuitState::HalfOpen as u8, Ordering::Relaxed);
        cb.probe_in_flight.store(true, Ordering::Relaxed);
        cb.record_failure(1); // 4th open -> 80s
        assert_eq!(cb.calculate_backoff_delay(), 80);
    }

    #[test]
    fn test_backoff_is_capped_at_maximum() {
        let cb = CircuitBreaker::with_config(10, 50, 0.0);
        cb.record_failure(1); // 1st open -> 10s
        for _ in 0..5 {
            cb.state.store(CircuitState::HalfOpen as u8, Ordering::Relaxed);
            cb.probe_in_flight.store(true, Ordering::Relaxed);
            cb.record_failure(1);
        }
        assert_eq!(cb.calculate_backoff_delay(), 50);
    }

    #[test]
    fn test_jitter_bounds() {
        let base = 100u64;
        let jitter_ratio = 0.2f64; // ±20%
        let cb = CircuitBreaker::with_config(base, 1000, jitter_ratio);
        cb.consecutive_opens.store(1, Ordering::Relaxed);

        for _ in 0..50 {
            let delay = cb.calculate_backoff_delay();
            assert!(delay >= 80, "delay {} below lower bound 80", delay);
            assert!(delay <= 120, "delay {} above upper bound 120", delay);
        }
    }

    #[test]
    fn test_success_resets_counters() {
        let cb = CircuitBreaker::new(30);
        cb.record_failure(1);
        assert_eq!(cb.consecutive_opens(), 1);

        cb.record_success();
        assert_eq!(cb.get_state(), CircuitState::Closed);
        assert_eq!(cb.consecutive_failures(), 0);
        assert_eq!(cb.consecutive_opens(), 0);
    }

    #[test]
    fn test_blocked_requests_do_not_increase_failure_counts() {
        let cb = CircuitBreaker::new(30);
        cb.record_failure(1); // Open cycle 1
        assert_eq!(cb.consecutive_opens(), 1);

        // Blocked requests while Open
        cb.record_failure(5);
        cb.record_failure(5);
        assert_eq!(cb.consecutive_opens(), 1); // Remains 1
    }

    #[test]
    fn test_changing_recovery_settings_works_correctly() {
        let cb = CircuitBreaker::new(30);
        cb.set_recovery_secs(60);
        assert_eq!(cb.calculate_backoff_delay(), 60);
    }
}
