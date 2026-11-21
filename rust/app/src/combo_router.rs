use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_HEDGED_DELAY: Duration = Duration::from_millis(800);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComboStrategy {
    Fallback,
    RoundRobin,
    Sticky,
    Capacity,
    Fusion,
    HedgedRacing,
}

impl ComboStrategy {
    pub fn parse(s: &str) -> Self {
        let s_lower = s.to_lowercase();
        let trimmed = s_lower.trim();
        let base = trimmed
            .split(':')
            .next()
            .unwrap_or(trimmed)
            .split('=')
            .next()
            .unwrap_or(trimmed)
            .trim();
        match base {
            "round-robin" | "roundrobin" => Self::RoundRobin,
            "sticky" => Self::Sticky,
            "capacity" => Self::Capacity,
            "fusion" => Self::Fusion,
            "hedged" | "race" | "hedged-racing" | "hedged_racing" | "racing" => Self::HedgedRacing,
            _ => Self::Fallback,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fallback => "fallback",
            Self::RoundRobin => "round-robin",
            Self::Sticky => "sticky",
            Self::Capacity => "capacity",
            Self::Fusion => "fusion",
            Self::HedgedRacing => "hedged",
        }
    }

    pub fn is_hedged_racing(&self) -> bool {
        matches!(self, Self::HedgedRacing | Self::Fusion)
    }
}

pub struct InFlightGuard {
    tracker: Arc<ComboTracker>,
    candidate: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if let Some(val) = self.tracker.in_flight.get(&self.candidate) {
            val.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

pub struct ComboTracker {
    pub rr_counters: DashMap<String, AtomicUsize>,
    pub sticky_models: DashMap<String, String>,
    pub in_flight: DashMap<String, AtomicUsize>,
    pub recent_errors: DashMap<String, AtomicUsize>,
    pub circuit_breakers: Arc<crate::circuit_breaker::CircuitBreakerRegistry>,
}

impl Default for ComboTracker {
    fn default() -> Self {
        Self {
            rr_counters: DashMap::new(),
            sticky_models: DashMap::new(),
            in_flight: DashMap::new(),
            recent_errors: DashMap::new(),
            circuit_breakers: Arc::new(crate::circuit_breaker::CircuitBreakerRegistry::default()),
        }
    }
}

impl ComboTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn with_circuit_breakers(circuit_breakers: Arc<crate::circuit_breaker::CircuitBreakerRegistry>) -> Arc<Self> {
        Arc::new(Self {
            rr_counters: DashMap::new(),
            sticky_models: DashMap::new(),
            in_flight: DashMap::new(),
            recent_errors: DashMap::new(),
            circuit_breakers,
        })
    }

    /// Checks if a candidate is available, taking into account candidate and provider circuit breakers.
    pub fn is_candidate_available(&self, candidate: &str) -> bool {
        self.circuit_breakers.is_candidate_available(candidate)
    }

    /// Filters candidate list, skipping any candidates whose circuit breaker is tripped to Open.
    pub fn filter_available_candidates(&self, candidates: &[String]) -> Vec<String> {
        candidates
            .iter()
            .filter(|c| self.is_candidate_available(c))
            .cloned()
            .collect()
    }

    /// Track an in-flight request to a candidate model using an RAII guard.
    pub fn track_in_flight(self: &Arc<Self>, candidate: &str) -> InFlightGuard {
        let entry = self
            .in_flight
            .entry(candidate.to_string())
            .or_insert_with(|| AtomicUsize::new(0));
        entry.fetch_add(1, Ordering::SeqCst);

        InFlightGuard {
            tracker: Arc::clone(self),
            candidate: candidate.to_string(),
        }
    }

    pub fn get_in_flight(&self, candidate: &str) -> usize {
        self.in_flight
            .get(candidate)
            .map(|v| v.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    pub fn record_error(&self, candidate: &str) {
        self.circuit_breakers.record_failure(candidate);
        let entry = self
            .recent_errors
            .entry(candidate.to_string())
            .or_insert_with(|| AtomicUsize::new(0));
        entry.fetch_add(1, Ordering::SeqCst);
    }

    pub fn record_error_with_status(&self, candidate: &str, status: reqwest::StatusCode) {
        self.circuit_breakers.record_status(candidate, status);
        let entry = self
            .recent_errors
            .entry(candidate.to_string())
            .or_insert_with(|| AtomicUsize::new(0));
        entry.fetch_add(1, Ordering::SeqCst);
    }

    pub fn record_success(&self, combo_name: &str, candidate: &str) {
        self.circuit_breakers.record_success(candidate);
        // Clear or decrement recent errors on success
        if let Some(errs) = self.recent_errors.get(candidate) {
            errs.store(0, Ordering::SeqCst);
        }
        // Update sticky candidate for sticky routing
        self.sticky_models
            .insert(combo_name.to_string(), candidate.to_string());
    }

    /// Orders candidate models based on the combo strategy, skipping tripped Open providers.
    pub fn order_candidates(
        &self,
        combo_name: &str,
        candidates: &[String],
        strategy: ComboStrategy,
    ) -> Vec<String> {
        if candidates.is_empty() {
            return Vec::new();
        }

        // Filter out candidates whose circuit breaker is Open (immediately diverting traffic without socket connection)
        let available = self.filter_available_candidates(candidates);
        if available.is_empty() {
            return Vec::new();
        }

        match strategy {
            ComboStrategy::Fallback => available,

            ComboStrategy::RoundRobin => {
                let counter = self
                    .rr_counters
                    .entry(combo_name.to_string())
                    .or_insert_with(|| AtomicUsize::new(0));
                let idx = counter.fetch_add(1, Ordering::SeqCst) % available.len();
                let mut reordered = Vec::with_capacity(available.len());
                reordered.extend_from_slice(&available[idx..]);
                reordered.extend_from_slice(&available[..idx]);
                reordered
            }

            ComboStrategy::Sticky => {
                if let Some(sticky) = self.sticky_models.get(combo_name) {
                    if let Some(pos) = available.iter().position(|c| c == sticky.value()) {
                        let mut reordered = Vec::with_capacity(available.len());
                        reordered.push(available[pos].clone());
                        for (i, c) in available.iter().enumerate() {
                            if i != pos {
                                reordered.push(c.clone());
                            }
                        }
                        return reordered;
                    }
                }
                available
            }

            ComboStrategy::Capacity => {
                // Capacity-aware: sort candidates by (in_flight_load * 1000 + recent_errors * 100)
                let mut scored: Vec<(usize, String)> = available
                    .iter()
                    .map(|c| {
                        let in_f = self.get_in_flight(c);
                        let errs = self
                            .recent_errors
                            .get(c)
                            .map(|v| v.load(Ordering::SeqCst))
                            .unwrap_or(0);
                        let score = in_f * 1000 + errs * 100;
                        (score, c.clone())
                    })
                    .collect();
                scored.sort_by_key(|(score, _)| *score);
                scored.into_iter().map(|(_, c)| c).collect()
            }

            ComboStrategy::Fusion | ComboStrategy::HedgedRacing => {
                // Fusion and HedgedRacing preserve defined candidate order among available candidates
                available
            }
        }
    }
}

/// Resolves the hedged speculative dispatch delay.
/// Priority:
/// 1. Strategy parameter if encoded as "hedged:<ms>" or "race:<ms>" or "hedged=<ms>"
/// 2. Match rules JSON field "hedgedDelayMs", "hedged_delay_ms", "hedgedDelay", or "hedged_delay"
/// 3. Environment variable `HEDGED_DELAY_MS` or `HEDGED_DELAY`
/// 4. Default: 800ms
pub fn resolve_hedged_delay(strategy_str: Option<&str>, match_rules: Option<&str>) -> Duration {
    // 1. Check strategy string format like "hedged:500" or "race:300"
    if let Some(s) = strategy_str {
        if let Some((_, delay_part)) = s.split_once(':').or_else(|| s.split_once('=')) {
            let digits: String = delay_part.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(ms) = digits.parse::<u64>() {
                if ms > 0 {
                    return Duration::from_millis(ms);
                }
            }
        }
    }

    // 2. Check match_rules JSON (e.g. {"hedgedDelayMs": 500} or {"hedged_delay": 500})
    if let Some(rules) = match_rules {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(rules) {
            let delay_val = v
                .get("hedgedDelayMs")
                .or_else(|| v.get("hedged_delay_ms"))
                .or_else(|| v.get("hedgedDelay"))
                .or_else(|| v.get("hedged_delay"));
            if let Some(num) = delay_val.and_then(|v| v.as_u64()) {
                if num > 0 {
                    return Duration::from_millis(num);
                }
            }
        }
    }

    // 3. Check environment variable HEDGED_DELAY_MS or HEDGED_DELAY
    if let Ok(env_val) = std::env::var("HEDGED_DELAY_MS").or_else(|_| std::env::var("HEDGED_DELAY")) {
        let digits: String = env_val.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(ms) = digits.parse::<u64>() {
            if ms > 0 {
                return Duration::from_millis(ms);
            }
        }
    }

    // 4. Default: 800ms
    DEFAULT_HEDGED_DELAY
}

/// Executes a hedged speculative race between primary and secondary candidates.
///
/// Coordination flow:
/// - Dispatches request to primary candidate.
/// - If primary delivers successful response before hedged_delay, it wins immediately;
///   secondary is never dispatched.
/// - If primary fails before hedged_delay, secondary candidate is dispatched immediately
///   without waiting for hedged_delay to finish.
/// - If primary does not return within hedged_delay, secondary candidate is dispatched
///   speculatively in parallel.
/// - Whichever candidate delivers a successful response first wins; the slower candidate
///   is cancelled immediately via CancellationToken and dropping its future.
/// - If both fail, the last failure error is returned.
pub async fn execute_hedged_race<F, Fut, T, E>(
    primary: String,
    secondary: String,
    hedged_delay: Duration,
    runner: F,
) -> Result<(String, T), E>
where
    F: Fn(String, CancellationToken) -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let token_primary = CancellationToken::new();
    let token_secondary = CancellationToken::new();

    let fut_primary = runner(primary.clone(), token_primary.clone());
    tokio::pin!(fut_primary);

    // If hedged_delay is zero, launch secondary immediately
    if hedged_delay.is_zero() {
        let fut_secondary = runner(secondary.clone(), token_secondary.clone());
        tokio::pin!(fut_secondary);
        return race_two_candidates(
            primary,
            secondary,
            fut_primary,
            fut_secondary,
            token_primary,
            token_secondary,
        )
        .await;
    }

    let delay = tokio::time::sleep(hedged_delay);
    tokio::pin!(delay);

    let mut primary_completed = false;
    let mut primary_ok = None;

    tokio::select! {
        res = &mut fut_primary => {
            primary_completed = true;
            if let Ok(val) = res {
                primary_ok = Some(val);
            }
        }
        _ = &mut delay => {
            // hedged_delay elapsed while primary is still in flight
        }
    }

    if primary_completed {
        if let Some(val) = primary_ok {
            // Primary won before hedged_delay! Secondary is never dispatched.
            return Ok((primary, val));
        }
        // Primary failed early before hedged_delay.
        // Speculatively dispatch secondary immediately without waiting!
        let fut_secondary = runner(secondary.clone(), token_secondary.clone());
        return match fut_secondary.await {
            Ok(val) => Ok((secondary, val)),
            Err(e) => Err(e),
        };
    }

    // Speculative parallel request to secondary candidate
    let fut_secondary = runner(secondary.clone(), token_secondary.clone());
    tokio::pin!(fut_secondary);

    race_two_candidates(
        primary,
        secondary,
        fut_primary,
        fut_secondary,
        token_primary,
        token_secondary,
    )
    .await
}

async fn race_two_candidates<Fut1, Fut2, T, E>(
    primary: String,
    secondary: String,
    mut fut_primary: std::pin::Pin<&mut Fut1>,
    mut fut_secondary: std::pin::Pin<&mut Fut2>,
    token_primary: CancellationToken,
    token_secondary: CancellationToken,
) -> Result<(String, T), E>
where
    Fut1: Future<Output = Result<T, E>>,
    Fut2: Future<Output = Result<T, E>>,
{
    let mut primary_failed = false;
    let mut secondary_failed = false;

    loop {
        tokio::select! {
            res_p = fut_primary.as_mut(), if !primary_failed => {
                match res_p {
                    Ok(val) => {
                        token_secondary.cancel();
                        return Ok((primary, val));
                    }
                    Err(e) => {
                        primary_failed = true;
                        if secondary_failed {
                            return Err(e);
                        }
                    }
                }
            }
            res_s = fut_secondary.as_mut(), if !secondary_failed => {
                match res_s {
                    Ok(val) => {
                        token_primary.cancel();
                        return Ok((secondary, val));
                    }
                    Err(e) => {
                        secondary_failed = true;
                        if primary_failed {
                            return Err(e);
                        }
                    }
                }
            }
        }
    }
}

/// Automatically clusters available model names across providers into family groups.
pub fn cluster_models_by_family(all_models: &[String]) -> Vec<(String, String, Vec<String>)> {
    // Families: (slug, display_name, regex/patterns)
    let families = [
        (
            "claude-3-7-sonnet",
            "Auto: claude-3-7-sonnet",
            vec!["claude-3-7-sonnet", "claude-3.7-sonnet"],
        ),
        (
            "claude-3-5-sonnet",
            "Auto: claude-3-5-sonnet",
            vec!["claude-3-5-sonnet", "claude-3.5-sonnet"],
        ),
        (
            "claude-3-5-haiku",
            "Auto: claude-3-5-haiku",
            vec!["claude-3-5-haiku", "claude-3.5-haiku"],
        ),
        (
            "gpt-4o",
            "Auto: gpt-4o",
            vec!["gpt-4o"],
        ),
        (
            "gpt-4o-mini",
            "Auto: gpt-4o-mini",
            vec!["gpt-4o-mini"],
        ),
        (
            "o3-mini",
            "Auto: o3-mini",
            vec!["o3-mini"],
        ),
        (
            "deepseek-chat",
            "Auto: deepseek-chat",
            vec!["deepseek-chat", "deepseek-v3"],
        ),
        (
            "deepseek-reasoner",
            "Auto: deepseek-reasoner",
            vec!["deepseek-reasoner", "deepseek-r1"],
        ),
        (
            "qwen-2.5-coder",
            "Auto: qwen-2.5-coder",
            vec!["qwen-2.5-coder", "qwen2.5-coder"],
        ),
        (
            "gemini-2.0-flash",
            "Auto: gemini-2.0-flash",
            vec!["gemini-2.0-flash", "gemini-2-flash"],
        ),
    ];

    let mut result = Vec::new();

    for (slug, name, patterns) in families {
        let mut matched = Vec::new();
        for m in all_models {
            let lower = m.to_lowercase();
            // Negative filter: don't match gpt-4o for gpt-4o-mini
            if slug == "gpt-4o" && lower.contains("mini") {
                continue;
            }
            if patterns.iter().any(|pat| lower.contains(pat)) && !matched.contains(m) {
                matched.push(m.clone());
            }
        }
        if matched.len() >= 2 {
            result.push((slug.to_string(), name.to_string(), matched));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_combo_strategy_parse() {
        assert_eq!(ComboStrategy::parse("fallback"), ComboStrategy::Fallback);
        assert_eq!(ComboStrategy::parse("round-robin"), ComboStrategy::RoundRobin);
        assert_eq!(ComboStrategy::parse("roundrobin"), ComboStrategy::RoundRobin);
        assert_eq!(ComboStrategy::parse("sticky"), ComboStrategy::Sticky);
        assert_eq!(ComboStrategy::parse("capacity"), ComboStrategy::Capacity);
        assert_eq!(ComboStrategy::parse("fusion"), ComboStrategy::Fusion);
        assert_eq!(ComboStrategy::parse("unknown"), ComboStrategy::Fallback);
    }

    #[test]
    fn test_round_robin_rotation() {
        let tracker = ComboTracker::new();
        let candidates = vec![
            "model-a".to_string(),
            "model-b".to_string(),
            "model-c".to_string(),
        ];

        let call1 = tracker.order_candidates("combo1", &candidates, ComboStrategy::RoundRobin);
        assert_eq!(call1, vec!["model-a", "model-b", "model-c"]);

        let call2 = tracker.order_candidates("combo1", &candidates, ComboStrategy::RoundRobin);
        assert_eq!(call2, vec!["model-b", "model-c", "model-a"]);

        let call3 = tracker.order_candidates("combo1", &candidates, ComboStrategy::RoundRobin);
        assert_eq!(call3, vec!["model-c", "model-a", "model-b"]);

        let call4 = tracker.order_candidates("combo1", &candidates, ComboStrategy::RoundRobin);
        assert_eq!(call4, vec!["model-a", "model-b", "model-c"]);
    }

    #[test]
    fn test_sticky_routing() {
        let tracker = ComboTracker::new();
        let candidates = vec![
            "model-1".to_string(),
            "model-2".to_string(),
            "model-3".to_string(),
        ];

        // Before any recorded success, defaults to candidate order
        let init = tracker.order_candidates("my-combo", &candidates, ComboStrategy::Sticky);
        assert_eq!(init, vec!["model-1", "model-2", "model-3"]);

        // When model-2 succeeds, sticky sticks to model-2
        tracker.record_success("my-combo", "model-2");
        let sticky = tracker.order_candidates("my-combo", &candidates, ComboStrategy::Sticky);
        assert_eq!(sticky[0], "model-2");
        assert_eq!(sticky.len(), 3);

        // When model-3 succeeds, sticky updates to model-3
        tracker.record_success("my-combo", "model-3");
        let updated = tracker.order_candidates("my-combo", &candidates, ComboStrategy::Sticky);
        assert_eq!(updated[0], "model-3");
    }

    #[test]
    fn test_capacity_steering_and_in_flight_guard() {
        let tracker = ComboTracker::new();
        let candidates = vec![
            "node-fast".to_string(),
            "node-busy".to_string(),
            "node-erroring".to_string(),
        ];

        // Simulate node-busy having in-flight requests
        {
            let _guard1 = tracker.track_in_flight("node-busy");
            let _guard2 = tracker.track_in_flight("node-busy");
            assert_eq!(tracker.get_in_flight("node-busy"), 2);

            // Record errors on node-erroring
            tracker.record_error("node-erroring");

            let ordered = tracker.order_candidates("my-combo", &candidates, ComboStrategy::Capacity);
            // node-fast has score 0, node-erroring has score 100, node-busy has score 2000
            assert_eq!(ordered[0], "node-fast");
            assert_eq!(ordered[1], "node-erroring");
            assert_eq!(ordered[2], "node-busy");
        }

        // After guards drop, in-flight counter decrements automatically
        assert_eq!(tracker.get_in_flight("node-busy"), 0);
    }

    #[test]
    fn test_cluster_models_by_family() {
        let models = vec![
            "anthropic/claude-3-5-sonnet".to_string(),
            "openrouter/anthropic/claude-3.5-sonnet".to_string(),
            "openai/gpt-4o".to_string(),
            "azure/gpt-4o".to_string(),
            "openai/gpt-4o-mini".to_string(),
            "azure/gpt-4o-mini".to_string(),
            "single-model-without-cluster".to_string(),
        ];

        let clusters = cluster_models_by_family(&models);
        assert!(clusters.iter().any(|(slug, _, c)| slug == "claude-3-5-sonnet" && c.len() == 2));
        assert!(clusters.iter().any(|(slug, _, c)| slug == "gpt-4o" && c.len() == 2));
        assert!(clusters.iter().any(|(slug, _, c)| slug == "gpt-4o-mini" && c.len() == 2));
        assert!(!clusters.iter().any(|(slug, _, _)| slug == "single-model-without-cluster"));
    }

    #[test]
    fn test_combo_strategy_parse_hedged_and_race() {
        assert_eq!(ComboStrategy::parse("hedged"), ComboStrategy::HedgedRacing);
        assert_eq!(ComboStrategy::parse("race"), ComboStrategy::HedgedRacing);
        assert_eq!(ComboStrategy::parse("hedged-racing"), ComboStrategy::HedgedRacing);
        assert_eq!(ComboStrategy::parse("hedged_racing"), ComboStrategy::HedgedRacing);
        assert_eq!(ComboStrategy::parse("racing"), ComboStrategy::HedgedRacing);
        assert_eq!(ComboStrategy::parse("hedged:500"), ComboStrategy::HedgedRacing);
        assert_eq!(ComboStrategy::parse("race:250ms"), ComboStrategy::HedgedRacing);
        assert_eq!(ComboStrategy::HedgedRacing.as_str(), "hedged");
        assert!(ComboStrategy::HedgedRacing.is_hedged_racing());
    }

    #[test]
    fn test_resolve_hedged_delay_all_sources() {
        // Default 800ms
        assert_eq!(resolve_hedged_delay(None, None), Duration::from_millis(800));
        assert_eq!(resolve_hedged_delay(Some("hedged"), None), Duration::from_millis(800));
        assert_eq!(resolve_hedged_delay(Some("race"), None), Duration::from_millis(800));

        // Strategy string parameters
        assert_eq!(resolve_hedged_delay(Some("hedged:350"), None), Duration::from_millis(350));
        assert_eq!(resolve_hedged_delay(Some("race:120ms"), None), Duration::from_millis(120));
        assert_eq!(resolve_hedged_delay(Some("hedged=500"), None), Duration::from_millis(500));

        // match_rules JSON
        assert_eq!(
            resolve_hedged_delay(Some("hedged"), Some(r#"{"hedgedDelayMs": 450}"#)),
            Duration::from_millis(450)
        );
        assert_eq!(
            resolve_hedged_delay(None, Some(r#"{"hedged_delay": 600}"#)),
            Duration::from_millis(600)
        );

        // Precedence: strategy string > match_rules
        assert_eq!(
            resolve_hedged_delay(Some("hedged:250"), Some(r#"{"hedgedDelayMs": 900}"#)),
            Duration::from_millis(250)
        );
    }

    #[tokio::test]
    async fn test_hedged_race_primary_fast_wins() {
        let runner = |cand: String, cancel_token: CancellationToken| async move {
            if cand == "primary" {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok("primary_response")
            } else {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(300)) => Ok("secondary_response"),
                    _ = cancel_token.cancelled() => Err("canceled"),
                }
            }
        };

        let res = execute_hedged_race(
            "primary".to_string(),
            "secondary".to_string(),
            Duration::from_millis(100),
            runner,
        )
        .await;

        assert!(res.is_ok());
        let (winner, val) = res.unwrap();
        assert_eq!(winner, "primary");
        assert_eq!(val, "primary_response");
    }

    #[tokio::test]
    async fn test_hedged_race_secondary_wins_and_primary_canceled() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Mutex;

        struct DropGuard(Arc<AtomicBool>);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let primary_dropped = Arc::new(AtomicBool::new(false));
        let pd_clone = primary_dropped.clone();
        let primary_token_holder: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));
        let pth_clone = primary_token_holder.clone();

        let runner = move |cand: String, cancel_token: CancellationToken| {
            let pd = pd_clone.clone();
            let pth = pth_clone.clone();
            async move {
                if cand == "primary" {
                    *pth.lock().unwrap() = Some(cancel_token.clone());
                    let _guard = DropGuard(pd);
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(500)) => Ok("slow_primary"),
                        _ = cancel_token.cancelled() => Err("primary_canceled"),
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Ok("fast_secondary")
                }
            }
        };

        let res = execute_hedged_race(
            "primary".to_string(),
            "secondary".to_string(),
            Duration::from_millis(40),
            runner,
        )
        .await;

        assert!(res.is_ok());
        let (winner, val) = res.unwrap();
        assert_eq!(winner, "secondary");
        assert_eq!(val, "fast_secondary");

        // Verify primary was cancelled via CancellationToken AND dropped
        let token = primary_token_holder.lock().unwrap().clone().expect("Primary token captured");
        assert!(token.is_cancelled(), "Primary CancellationToken must be cancelled");
        assert!(primary_dropped.load(Ordering::SeqCst), "Primary connection future must be dropped");
    }

    #[tokio::test]
    async fn test_hedged_race_primary_wins_when_faster_than_secondary() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Mutex;

        struct DropGuard(Arc<AtomicBool>);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let secondary_dropped = Arc::new(AtomicBool::new(false));
        let sd_clone = secondary_dropped.clone();
        let secondary_token_holder: Arc<Mutex<Option<CancellationToken>>> = Arc::new(Mutex::new(None));
        let sth_clone = secondary_token_holder.clone();

        let runner = move |cand: String, cancel_token: CancellationToken| {
            let sd = sd_clone.clone();
            let sth = sth_clone.clone();
            async move {
                if cand == "primary" {
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    Ok("primary_win")
                } else {
                    *sth.lock().unwrap() = Some(cancel_token.clone());
                    let _guard = DropGuard(sd);
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(100)) => Ok("secondary_finish"),
                        _ = cancel_token.cancelled() => Err("secondary_canceled"),
                    }
                }
            }
        };

        let res = execute_hedged_race(
            "primary".to_string(),
            "secondary".to_string(),
            Duration::from_millis(30),
            runner,
        )
        .await;

        assert!(res.is_ok());
        let (winner, val) = res.unwrap();
        assert_eq!(winner, "primary");
        assert_eq!(val, "primary_win");

        // Verify secondary was cancelled via CancellationToken AND dropped
        let token = secondary_token_holder.lock().unwrap().clone().expect("Secondary token captured");
        assert!(token.is_cancelled(), "Secondary CancellationToken must be cancelled");
        assert!(secondary_dropped.load(Ordering::SeqCst), "Secondary connection future must be dropped");
    }

    #[tokio::test]
    async fn test_hedged_race_primary_fails_early_secondary_immediate() {
        let start = std::time::Instant::now();

        let runner = |cand: String, _cancel_token: CancellationToken| async move {
            if cand == "primary" {
                tokio::time::sleep(Duration::from_millis(15)).await;
                Err("primary_error")
            } else {
                tokio::time::sleep(Duration::from_millis(25)).await;
                Ok("secondary_ok")
            }
        };

        let res = execute_hedged_race(
            "primary".to_string(),
            "secondary".to_string(),
            Duration::from_millis(500),
            runner,
        )
        .await;

        let elapsed = start.elapsed();
        assert!(res.is_ok());
        let (winner, val) = res.unwrap();
        assert_eq!(winner, "secondary");
        assert_eq!(val, "secondary_ok");
        assert!(elapsed < Duration::from_millis(200), "Secondary must be dispatched immediately upon primary failure");
    }

    #[tokio::test]
    async fn test_hedged_race_both_fail() {
        let runner = |cand: String, _cancel_token: CancellationToken| async move {
            let res: Result<&'static str, &'static str> = if cand == "primary" {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Err("primary_fail")
            } else {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Err("secondary_fail")
            };
            res
        };

        let res = execute_hedged_race(
            "primary".to_string(),
            "secondary".to_string(),
            Duration::from_millis(10),
            runner,
        )
        .await;

        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_hedged_race_future_drop_cancels_in_flight() {
        use std::sync::atomic::AtomicBool;

        struct DropGuard(Arc<AtomicBool>);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let primary_dropped = Arc::new(AtomicBool::new(false));
        let pd_clone = primary_dropped.clone();

        let runner = move |cand: String, _cancel_token: CancellationToken| {
            let pd = pd_clone.clone();
            async move {
                let res: Result<&'static str, &'static str> = if cand == "primary" {
                    let _guard = DropGuard(pd);
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    Ok("primary_res")
                } else {
                    tokio::time::sleep(Duration::from_millis(15)).await;
                    Ok("secondary_res")
                };
                res
            }
        };

        let res = execute_hedged_race(
            "primary".to_string(),
            "secondary".to_string(),
            Duration::from_millis(20),
            runner,
        )
        .await;

        assert!(res.is_ok());
        let (winner, _) = res.unwrap();
        assert_eq!(winner, "secondary");
        assert!(primary_dropped.load(Ordering::SeqCst), "Primary future must be dropped on secondary win");
    }

    #[test]
    fn test_order_candidates_skips_tripped_open_candidates() {
        let tracker = ComboTracker::new();
        let candidates = vec![
            "cand-1".to_string(),
            "cand-2".to_string(),
            "cand-3".to_string(),
        ];

        // Normal state: all 3 available
        let ordered = tracker.order_candidates("test_combo", &candidates, ComboStrategy::Fallback);
        assert_eq!(ordered, candidates);

        // Trip cand-1
        tracker.circuit_breakers.trip("cand-1");
        assert!(tracker.circuit_breakers.is_open("cand-1"));

        // order_candidates must skip cand-1 and immediately divert to cand-2 and cand-3
        let ordered = tracker.order_candidates("test_combo", &candidates, ComboStrategy::Fallback);
        assert_eq!(ordered, vec!["cand-2".to_string(), "cand-3".to_string()]);

        // Trip cand-2 as well
        tracker.circuit_breakers.trip("cand-2");
        let ordered = tracker.order_candidates("test_combo", &candidates, ComboStrategy::Fallback);
        assert_eq!(ordered, vec!["cand-3".to_string()]);

        // Reset cand-1
        tracker.circuit_breakers.reset("cand-1");
        let ordered = tracker.order_candidates("test_combo", &candidates, ComboStrategy::Fallback);
        assert_eq!(ordered, vec!["cand-1".to_string(), "cand-3".to_string()]);
    }

    #[test]
    fn test_order_candidates_returns_empty_when_all_tripped() {
        let tracker = ComboTracker::new();
        let candidates = vec!["cand-1".to_string(), "cand-2".to_string()];

        tracker.circuit_breakers.trip("cand-1");
        tracker.circuit_breakers.trip("cand-2");

        let ordered = tracker.order_candidates("test_combo", &candidates, ComboStrategy::Fallback);
        assert!(ordered.is_empty(), "All candidates tripped -> return empty immediately without socket connection");
    }

    #[test]
    fn test_order_candidates_half_open_single_canary_allocation() {
        let cb_config = crate::circuit_breaker::CircuitBreakerConfig {
            failure_threshold: 3,
            cooldown_duration: Duration::from_millis(50),
            rolling_window: Duration::from_secs(60),
            canary_timeout: Duration::from_secs(30),
        };
        let registry = Arc::new(crate::circuit_breaker::CircuitBreakerRegistry::new(cb_config));
        let tracker = ComboTracker::with_circuit_breakers(registry);
        let candidates = vec!["cand-1".to_string(), "cand-2".to_string()];

        // Trip cand-1
        tracker.circuit_breakers.trip("cand-1");
        assert!(!tracker.circuit_breakers.is_available("cand-1"));

        // Wait for cooldown to elapse
        std::thread::sleep(Duration::from_millis(60));

        // 1st call to order_candidates: transitions cand-1 to HalfOpen and allocates 1 canary probe
        let ordered_first = tracker.order_candidates("test_combo", &candidates, ComboStrategy::Fallback);
        assert_eq!(ordered_first, vec!["cand-1".to_string(), "cand-2".to_string()]);

        // 2nd concurrent call to order_candidates: cand-1 canary is already in flight,
        // so traffic is diverted to cand-2!
        let ordered_second = tracker.order_candidates("test_combo", &candidates, ComboStrategy::Fallback);
        assert_eq!(ordered_second, vec!["cand-2".to_string()]);

        // When canary succeeds:
        tracker.record_success("test_combo", "cand-1");
        assert_eq!(tracker.circuit_breakers.state("cand-1"), crate::circuit_breaker::CircuitState::Closed);

        // Now both candidates are healthy again
        let ordered_after_success = tracker.order_candidates("test_combo", &candidates, ComboStrategy::Fallback);
        assert_eq!(ordered_after_success, vec!["cand-1".to_string(), "cand-2".to_string()]);
    }

    #[test]
    fn test_combo_tracker_consecutive_failures_trips_circuit_breaker() {
        let cb_config = crate::circuit_breaker::CircuitBreakerConfig {
            failure_threshold: 3,
            cooldown_duration: Duration::from_secs(15),
            rolling_window: Duration::from_secs(60),
            canary_timeout: Duration::from_secs(30),
        };
        let registry = Arc::new(crate::circuit_breaker::CircuitBreakerRegistry::new(cb_config));
        let tracker = ComboTracker::with_circuit_breakers(registry);
        let candidates = vec!["failing-provider".to_string(), "backup-provider".to_string()];

        // 2 errors do not trip
        tracker.record_error("failing-provider");
        tracker.record_error("failing-provider");
        assert_eq!(tracker.order_candidates("test_combo", &candidates, ComboStrategy::Fallback).len(), 2);

        // 3rd error trips to Open
        tracker.record_error("failing-provider");
        assert!(tracker.circuit_breakers.is_open("failing-provider"));

        // Next order_candidates skips failing-provider
        let ordered = tracker.order_candidates("test_combo", &candidates, ComboStrategy::Fallback);
        assert_eq!(ordered, vec!["backup-provider".to_string()]);
    }
}
