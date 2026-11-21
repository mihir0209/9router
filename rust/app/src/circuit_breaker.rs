use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use dashmap::DashMap;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

/// Represents the three states of the Circuit Breaker pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CircuitState {
    /// Normal operation: requests pass through. Failures are tracked.
    Closed,
    /// Upstream is failing: requests are blocked immediately without socket connection.
    Open,
    /// Cooldown elapsed: exactly one canary probe request is permitted to test upstream recovery.
    HalfOpen,
}

impl CircuitState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half_open",
        }
    }
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Configuration parameters for a Circuit Breaker.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Consecutive failure threshold before tripping from Closed to Open (default: 3).
    pub failure_threshold: usize,
    /// Cooldown period while Open before transitioning to HalfOpen (default: 15 seconds).
    pub cooldown_duration: Duration,
    /// Rolling time window within which failures must occur to trigger tripping (default: 60 seconds).
    pub rolling_window: Duration,
    /// Maximum duration a canary probe request may stay in flight before timing out (default: 30 seconds).
    pub canary_timeout: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        let failure_threshold = std::env::var("CIRCUIT_BREAKER_FAILURE_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);

        let cooldown_duration = std::env::var("CIRCUIT_BREAKER_COOLDOWN_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(15));

        let rolling_window = std::env::var("CIRCUIT_BREAKER_ROLLING_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(60));

        Self {
            failure_threshold,
            cooldown_duration,
            rolling_window,
            canary_timeout: Duration::from_secs(30),
        }
    }
}

/// Checks whether an HTTP status code indicates an upstream failure that should trip the breaker.
/// 5xx server errors, 429 Too Many Requests, and 408 Request Timeout trip the breaker.
/// 4xx client errors (400, 401, 403, 404, etc.) do NOT trip the breaker.
pub fn is_trip_status(status: StatusCode) -> bool {
    status.is_server_error()
        || status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
}

#[derive(Debug)]
struct CircuitBreakerInner {
    state: CircuitState,
    consecutive_failures: usize,
    failure_timestamps: Vec<Instant>,
    opened_at: Option<Instant>,
    canary_in_flight: bool,
    canary_started_at: Option<Instant>,
}

impl Default for CircuitBreakerInner {
    fn default() -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            failure_timestamps: Vec::new(),
            opened_at: None,
            canary_in_flight: false,
            canary_started_at: None,
        }
    }
}

/// An upstream Circuit Breaker implementing the 3-state finite state machine
/// with canary health probing and rolling window failure tracking.
#[derive(Debug)]
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    inner: Mutex<CircuitBreakerInner>,
    total_trips: AtomicUsize,
    total_probes: AtomicUsize,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(CircuitBreakerInner::default()),
            total_trips: AtomicUsize::new(0),
            total_probes: AtomicUsize::new(0),
        }
    }

    pub fn config(&self) -> &CircuitBreakerConfig {
        &self.config
    }

    pub fn total_trips(&self) -> usize {
        self.total_trips.load(Ordering::Relaxed)
    }

    pub fn total_probes(&self) -> usize {
        self.total_probes.load(Ordering::Relaxed)
    }

    /// Returns the current state at the specified instant, taking time-based cooldown transitions into account.
    pub fn state_at(&self, now: Instant) -> CircuitState {
        let mut inner = self.inner.lock().unwrap();
        if inner.state == CircuitState::Open {
            if let Some(opened_at) = inner.opened_at {
                if now.duration_since(opened_at) >= self.config.cooldown_duration {
                    inner.state = CircuitState::HalfOpen;
                }
            }
        }
        inner.state
    }

    /// Returns the current state taking time-based transitions into account.
    pub fn state(&self) -> CircuitState {
        self.state_at(Instant::now())
    }

    /// Returns whether the circuit breaker is actively Open (blocking traffic).
    pub fn is_open(&self) -> bool {
        self.state() == CircuitState::Open
    }

    /// Returns whether the circuit breaker is Closed (healthy).
    pub fn is_closed(&self) -> bool {
        self.state() == CircuitState::Closed
    }

    /// Returns whether the circuit breaker is in HalfOpen (probing).
    pub fn is_half_open(&self) -> bool {
        self.state() == CircuitState::HalfOpen
    }

    /// Checks if the upstream provider is available for requests at the specified time.
    ///
    /// Semantics:
    /// - Closed: Always returns `true`.
    /// - Open: If cooldown has elapsed, transitions to HalfOpen, allocates 1 canary probe permit, and returns `true`.
    ///   Otherwise returns `false` (diverting traffic without socket connection).
    /// - HalfOpen: If canary probe is not yet in flight, allocates the permit and returns `true`.
    ///   If canary probe is already in flight, returns `false` to avoid overwhelming the recovering upstream.
    pub fn is_available_at(&self, now: Instant) -> bool {
        let mut inner = self.inner.lock().unwrap();
        match inner.state {
            CircuitState::Closed => true,

            CircuitState::Open => {
                if let Some(opened_at) = inner.opened_at {
                    if now.duration_since(opened_at) >= self.config.cooldown_duration {
                        // Cooldown elapsed: transition to HalfOpen and allow 1 canary probe
                        inner.state = CircuitState::HalfOpen;
                        inner.canary_in_flight = true;
                        inner.canary_started_at = Some(now);
                        self.total_probes.fetch_add(1, Ordering::Relaxed);
                        tracing::info!(
                            "CircuitBreaker cooldown of {:?} elapsed: transitioning to HalfOpen (canary probe allocated)",
                            self.config.cooldown_duration
                        );
                        return true;
                    }
                }
                false
            }

            CircuitState::HalfOpen => {
                if !inner.canary_in_flight {
                    inner.canary_in_flight = true;
                    inner.canary_started_at = Some(now);
                    self.total_probes.fetch_add(1, Ordering::Relaxed);
                    tracing::info!("CircuitBreaker in HalfOpen: canary probe allocated");
                    true
                } else if let Some(started_at) = inner.canary_started_at {
                    if now.duration_since(started_at) > self.config.canary_timeout {
                        // In-flight canary probe timed out: re-arm and permit new canary probe
                        inner.canary_started_at = Some(now);
                        self.total_probes.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!("CircuitBreaker canary probe timed out; re-arming new canary probe");
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
        }
    }

    /// Checks if the upstream provider is available for requests right now.
    pub fn is_available(&self) -> bool {
        self.is_available_at(Instant::now())
    }

    /// Non-mutating probe check: returns whether a request would be permitted, without claiming the canary permit.
    pub fn peek_available(&self) -> bool {
        let now = Instant::now();
        let inner = self.inner.lock().unwrap();
        match inner.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                if let Some(opened_at) = inner.opened_at {
                    now.duration_since(opened_at) >= self.config.cooldown_duration
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                !inner.canary_in_flight
                    || inner
                        .canary_started_at
                        .map(|s| now.duration_since(s) > self.config.canary_timeout)
                        .unwrap_or(false)
            }
        }
    }

    /// Records a successful upstream response.
    ///
    /// Semantics:
    /// - HalfOpen: Canary succeeded! Resets state to Closed, clears failure counters, and re-enables full traffic.
    /// - Closed: Resets consecutive failure counter and clears failure timestamps.
    /// - Open: Resets state to Closed (e.g. out-of-band manual recovery).
    pub fn record_success_at(&self, _now: Instant) {
        let mut inner = self.inner.lock().unwrap();
        let previous_state = inner.state;
        inner.consecutive_failures = 0;
        inner.failure_timestamps.clear();
        inner.opened_at = None;
        inner.canary_in_flight = false;
        inner.canary_started_at = None;
        inner.state = CircuitState::Closed;

        if previous_state != CircuitState::Closed {
            tracing::info!(
                "CircuitBreaker canary probe succeeded! State transitioned from {} to Closed",
                previous_state
            );
        }
    }

    pub fn record_success(&self) {
        self.record_success_at(Instant::now());
    }

    /// Records an upstream failure at the specified time.
    ///
    /// Semantics:
    /// - HalfOpen: Canary failed! Immediately returns state to Open with a fresh cooldown timer.
    /// - Closed: Prunes failure timestamps older than `rolling_window`. Increments consecutive failures.
    ///   If consecutive failures >= `failure_threshold`, trips state to Open.
    /// - Open: Refreshes `opened_at` timestamp.
    ///
    /// Returns `true` if this failure caused the breaker to transition to Open.
    pub fn record_failure_at(&self, now: Instant) -> bool {
        let mut inner = self.inner.lock().unwrap();
        match inner.state {
            CircuitState::HalfOpen => {
                inner.state = CircuitState::Open;
                inner.opened_at = Some(now);
                inner.canary_in_flight = false;
                inner.canary_started_at = None;
                inner.consecutive_failures = 0;
                inner.failure_timestamps.clear();
                self.total_trips.fetch_add(1, Ordering::Relaxed);
                tracing::warn!("CircuitBreaker canary probe failed! State transitioned back to Open");
                true
            }

            CircuitState::Closed => {
                // Prune timestamps outside rolling window
                inner.failure_timestamps.retain(|&t| {
                    now.duration_since(t) <= self.config.rolling_window
                });
                inner.failure_timestamps.push(now);
                inner.consecutive_failures += 1;

                if inner.consecutive_failures >= self.config.failure_threshold
                    && inner.failure_timestamps.len() >= self.config.failure_threshold
                {
                    inner.state = CircuitState::Open;
                    inner.opened_at = Some(now);
                    inner.canary_in_flight = false;
                    inner.canary_started_at = None;
                    inner.consecutive_failures = 0;
                    inner.failure_timestamps.clear();
                    self.total_trips.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        "CircuitBreaker tripped to Open after {} consecutive failures within {:?} rolling window",
                        self.config.failure_threshold,
                        self.config.rolling_window
                    );
                    true
                } else {
                    false
                }
            }

            CircuitState::Open => {
                inner.opened_at = Some(now);
                false
            }
        }
    }

    pub fn record_failure(&self) -> bool {
        self.record_failure_at(Instant::now())
    }

    /// Records an upstream response status code.
    /// If the status is 2xx, records success.
    /// If the status is 5xx or 429, records failure (and trips if threshold reached).
    /// If the status is 4xx client error, it is ignored (does not trip breaker or reset failures).
    pub fn record_status(&self, status: StatusCode) -> bool {
        if status.is_success() {
            self.record_success();
            false
        } else if is_trip_status(status) {
            self.record_failure()
        } else {
            false
        }
    }

    /// Manually trips the circuit breaker to Open state at the specified time.
    pub fn trip_at(&self, now: Instant) {
        let mut inner = self.inner.lock().unwrap();
        inner.state = CircuitState::Open;
        inner.opened_at = Some(now);
        inner.canary_in_flight = false;
        inner.canary_started_at = None;
        inner.consecutive_failures = 0;
        inner.failure_timestamps.clear();
        self.total_trips.fetch_add(1, Ordering::Relaxed);
    }

    /// Manually trips the circuit breaker to Open state.
    pub fn trip(&self) {
        self.trip_at(Instant::now());
    }

    /// Manually resets the circuit breaker to Closed state.
    pub fn reset(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.state = CircuitState::Closed;
        inner.opened_at = None;
        inner.canary_in_flight = false;
        inner.canary_started_at = None;
        inner.consecutive_failures = 0;
        inner.failure_timestamps.clear();
    }

    /// Manually transitions the circuit breaker to HalfOpen state for testing.
    pub fn half_open(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.state = CircuitState::HalfOpen;
        inner.canary_in_flight = false;
        inner.canary_started_at = None;
    }

    /// Releases an in-flight canary probe permit (e.g. if the probe was cancelled before sending).
    pub fn release_canary(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.state == CircuitState::HalfOpen {
            inner.canary_in_flight = false;
            inner.canary_started_at = None;
        }
    }

    pub fn consecutive_failures(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.consecutive_failures
    }
}

/// Thread-safe registry that manages circuit breakers for upstream providers and candidate models.
#[derive(Clone)]
pub struct CircuitBreakerRegistry {
    breakers: Arc<DashMap<String, Arc<CircuitBreaker>>>,
    config: CircuitBreakerConfig,
}

impl Default for CircuitBreakerRegistry {
    fn default() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }
}

impl CircuitBreakerRegistry {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            breakers: Arc::new(DashMap::new()),
            config,
        }
    }

    pub fn config(&self) -> &CircuitBreakerConfig {
        &self.config
    }

    /// Retrieves an existing circuit breaker or creates a new one with the default configuration.
    pub fn get_or_create(&self, key: &str) -> Arc<CircuitBreaker> {
        self.breakers
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(CircuitBreaker::new(self.config.clone())))
            .clone()
    }

    /// Retrieves an existing circuit breaker if already registered.
    pub fn get(&self, key: &str) -> Option<Arc<CircuitBreaker>> {
        self.breakers.get(key).map(|entry| Arc::clone(entry.value()))
    }

    /// Checks if a circuit breaker is actively Open for the given key.
    pub fn is_open(&self, key: &str) -> bool {
        if let Some(cb) = self.get(key) {
            cb.is_open()
        } else {
            false
        }
    }

    /// Checks if the upstream key is available for requests (allowing canary probe in HalfOpen).
    pub fn is_available(&self, key: &str) -> bool {
        self.get_or_create(key).is_available()
    }

    /// Checks if a candidate is available, taking into account candidate-level breakers,
    /// provider-prefix breakers (e.g. "openai" for "openai/gpt-4o"), and inferred providers.
    pub fn is_candidate_available(&self, candidate: &str) -> bool {
        // 1. Check the candidate's own breaker
        if !self.is_available(candidate) {
            return false;
        }

        // 2. If candidate has a provider prefix like "openai/gpt-4o", check provider breaker
        if let Some((prefix, _)) = candidate.split_once('/') {
            if !self.is_available(prefix) {
                return false;
            }
        }

        // 3. Known model family prefix inference
        if candidate.starts_with("gpt-") || candidate.starts_with("o1") || candidate.starts_with("o3") {
            if self.is_open("openai") {
                return false;
            }
        } else if candidate.starts_with("claude") {
            if self.is_open("anthropic") {
                return false;
            }
        } else if candidate.starts_with("gemini") {
            if self.is_open("vertex") || self.is_open("google") {
                return false;
            }
        }

        true
    }

    /// Records a success for the given key.
    pub fn record_success(&self, key: &str) {
        self.get_or_create(key).record_success();
    }

    /// Records a failure for the given key. Returns true if tripped to Open.
    pub fn record_failure(&self, key: &str) -> bool {
        self.get_or_create(key).record_failure()
    }

    /// Records an HTTP status code for the given key.
    pub fn record_status(&self, key: &str, status: StatusCode) -> bool {
        self.get_or_create(key).record_status(status)
    }

    /// Manually trips the breaker for the given key.
    pub fn trip(&self, key: &str) {
        self.get_or_create(key).trip();
    }

    /// Manually resets the breaker for the given key.
    pub fn reset(&self, key: &str) {
        self.get_or_create(key).reset();
    }

    /// Returns the current state of the breaker for the given key (defaulting to Closed if not tracked).
    pub fn state(&self, key: &str) -> CircuitState {
        if let Some(cb) = self.get(key) {
            cb.state()
        } else {
            CircuitState::Closed
        }
    }

    /// Returns all registered circuit breaker keys and their states.
    pub fn all_states(&self) -> Vec<(String, CircuitState)> {
        self.breakers
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().state()))
            .collect()
    }

    /// Clears all registered circuit breakers.
    pub fn clear(&self) {
        self.breakers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state_is_closed_and_available() {
        let cb = CircuitBreaker::default();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.is_closed());
        assert!(!cb.is_open());
        assert!(!cb.is_half_open());
        assert!(cb.is_available());
    }

    #[test]
    fn test_consecutive_failures_trip_to_open() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            cooldown_duration: Duration::from_secs(15),
            rolling_window: Duration::from_secs(60),
            canary_timeout: Duration::from_secs(30),
        };
        let cb = CircuitBreaker::new(config);
        let t0 = Instant::now();

        // 1st failure: 500
        assert!(!cb.record_failure_at(t0));
        assert_eq!(cb.state_at(t0), CircuitState::Closed);
        assert!(cb.is_available_at(t0));

        // 2nd failure: 429
        assert!(!cb.record_failure_at(t0 + Duration::from_secs(5)));
        assert_eq!(cb.state_at(t0 + Duration::from_secs(5)), CircuitState::Closed);
        assert!(cb.is_available_at(t0 + Duration::from_secs(5)));

        // 3rd failure: 503 -> trips to Open!
        assert!(cb.record_failure_at(t0 + Duration::from_secs(10)));
        assert_eq!(cb.state_at(t0 + Duration::from_secs(10)), CircuitState::Open);
        assert!(cb.is_open());
        assert_eq!(cb.total_trips(), 1);

        // While Open, is_available returns false (traffic diverted without socket connection)
        assert!(!cb.is_available_at(t0 + Duration::from_secs(12)));
        assert!(!cb.is_available_at(t0 + Duration::from_secs(24))); // 14s after trip < 15s cooldown
    }

    #[test]
    fn test_success_resets_consecutive_failures() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            cooldown_duration: Duration::from_secs(15),
            rolling_window: Duration::from_secs(60),
            canary_timeout: Duration::from_secs(30),
        };
        let cb = CircuitBreaker::new(config);
        let t0 = Instant::now();

        // 2 failures
        cb.record_failure_at(t0);
        cb.record_failure_at(t0 + Duration::from_secs(2));
        assert_eq!(cb.consecutive_failures(), 2);

        // Intervening success
        cb.record_success_at(t0 + Duration::from_secs(3));
        assert_eq!(cb.consecutive_failures(), 0);

        // Another failure should not trip because consecutive failures reset
        assert!(!cb.record_failure_at(t0 + Duration::from_secs(4)));
        assert_eq!(cb.state_at(t0 + Duration::from_secs(4)), CircuitState::Closed);
        assert!(cb.is_available_at(t0 + Duration::from_secs(4)));
    }

    #[test]
    fn test_client_error_statuses_do_not_trip() {
        let cb = CircuitBreaker::default();

        // 400 Bad Request, 401 Unauthorized, 404 Not Found do not trip
        assert!(!cb.record_status(StatusCode::BAD_REQUEST));
        assert!(!cb.record_status(StatusCode::UNAUTHORIZED));
        assert!(!cb.record_status(StatusCode::NOT_FOUND));
        assert_eq!(cb.consecutive_failures(), 0);
        assert!(cb.is_available());

        // 500, 502, 503, 429 DO count as trip statuses
        assert!(is_trip_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_trip_status(StatusCode::BAD_GATEWAY));
        assert!(is_trip_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_trip_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(!is_trip_status(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn test_rolling_window_prunes_stale_failures() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            cooldown_duration: Duration::from_secs(15),
            rolling_window: Duration::from_secs(20), // 20s rolling window
            canary_timeout: Duration::from_secs(30),
        };
        let cb = CircuitBreaker::new(config);
        let t0 = Instant::now();

        // Failure 1 at t=0
        cb.record_failure_at(t0);
        // Failure 2 at t=5s
        cb.record_failure_at(t0 + Duration::from_secs(5));

        // Failure 3 at t=30s (t=0 and t=5s are outside 20s rolling window!)
        assert!(!cb.record_failure_at(t0 + Duration::from_secs(30)));
        assert_eq!(cb.state_at(t0 + Duration::from_secs(30)), CircuitState::Closed);
    }

    #[test]
    fn test_cooldown_to_half_open_and_single_canary_probe() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            cooldown_duration: Duration::from_secs(15),
            rolling_window: Duration::from_secs(60),
            canary_timeout: Duration::from_secs(30),
        };
        let cb = CircuitBreaker::new(config);
        let t0 = Instant::now();

        // Trip to Open at t0
        cb.record_failure_at(t0);
        cb.record_failure_at(t0);
        cb.record_failure_at(t0);
        assert_eq!(cb.state_at(t0), CircuitState::Open);

        // Before cooldown (t0 + 10s): blocked
        assert!(!cb.is_available_at(t0 + Duration::from_secs(10)));

        // After cooldown (t0 + 15s): transitions to HalfOpen, exactly 1 canary probe allowed
        let t_probe = t0 + Duration::from_secs(15);
        assert!(cb.is_available_at(t_probe)); // Canary #1 allowed!
        assert_eq!(cb.state_at(t_probe), CircuitState::HalfOpen);
        assert_eq!(cb.total_probes(), 1);

        // Subsequent caller while canary is in-flight: BLOCKED!
        assert!(!cb.is_available_at(t_probe + Duration::from_millis(100)));
        assert!(!cb.is_available_at(t_probe + Duration::from_secs(1)));
    }

    #[test]
    fn test_canary_success_resets_to_closed() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            cooldown_duration: Duration::from_secs(15),
            rolling_window: Duration::from_secs(60),
            canary_timeout: Duration::from_secs(30),
        };
        let cb = CircuitBreaker::new(config);
        let t0 = Instant::now();

        // Trip to Open
        cb.trip_at(t0);
        assert!(cb.is_open());

        // Cooldown passes, canary allocated
        let t_probe = t0 + Duration::from_secs(15);
        assert!(cb.is_available_at(t_probe));
        assert_eq!(cb.state_at(t_probe), CircuitState::HalfOpen);

        // Canary probe succeeds!
        cb.record_success_at(t_probe + Duration::from_millis(500));
        assert_eq!(cb.state_at(t_probe + Duration::from_millis(500)), CircuitState::Closed);
        assert!(cb.is_closed());

        // Now all subsequent requests are allowed
        assert!(cb.is_available_at(t_probe + Duration::from_secs(1)));
        assert!(cb.is_available_at(t_probe + Duration::from_secs(2)));
    }

    #[test]
    fn test_canary_failure_returns_to_open() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            cooldown_duration: Duration::from_secs(15),
            rolling_window: Duration::from_secs(60),
            canary_timeout: Duration::from_secs(30),
        };
        let cb = CircuitBreaker::new(config);
        let t0 = Instant::now();

        // Trip to Open
        cb.trip_at(t0);

        // Cooldown passes, canary allocated
        let t_probe = t0 + Duration::from_secs(15);
        assert!(cb.is_available_at(t_probe));
        assert_eq!(cb.state_at(t_probe), CircuitState::HalfOpen);

        // Canary probe fails!
        let t_fail = t_probe + Duration::from_millis(500);
        assert!(cb.record_failure_at(t_fail));
        assert_eq!(cb.state_at(t_fail), CircuitState::Open);
        assert!(cb.is_open());

        // Requests blocked again for new cooldown
        assert!(!cb.is_available_at(t_fail + Duration::from_secs(1)));
        assert!(!cb.is_available_at(t_fail + Duration::from_secs(14)));

        // After second cooldown passes, new canary allowed
        assert!(cb.is_available_at(t_fail + Duration::from_secs(16)));
    }

    #[test]
    fn test_registry_provider_and_candidate_hierarchical_availability() {
        let registry = CircuitBreakerRegistry::default();

        // Normal state: everything available
        assert!(registry.is_candidate_available("openai/gpt-4o"));
        assert!(registry.is_candidate_available("anthropic/claude-3-5-sonnet"));
        assert!(registry.is_candidate_available("vertex/gemini-2.0-flash"));

        // Trip provider "openai"
        registry.trip("openai");
        assert!(registry.is_open("openai"));

        // openai/gpt-4o and gpt-4o are now unavailable because their provider is Open!
        assert!(!registry.is_candidate_available("openai/gpt-4o"));
        assert!(!registry.is_candidate_available("gpt-4o"));

        // anthropic and vertex candidates remain unaffected!
        assert!(registry.is_candidate_available("anthropic/claude-3-5-sonnet"));
        assert!(registry.is_candidate_available("vertex/gemini-2.0-flash"));

        // Reset openai provider
        registry.reset("openai");
        assert!(registry.is_candidate_available("openai/gpt-4o"));
    }
}
