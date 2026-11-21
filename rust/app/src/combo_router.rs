use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComboStrategy {
    Fallback,
    RoundRobin,
    Sticky,
    Capacity,
    Fusion,
}

impl ComboStrategy {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().trim() {
            "round-robin" | "roundrobin" => Self::RoundRobin,
            "sticky" => Self::Sticky,
            "capacity" => Self::Capacity,
            "fusion" => Self::Fusion,
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
        }
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

#[derive(Default)]
pub struct ComboTracker {
    pub rr_counters: DashMap<String, AtomicUsize>,
    pub sticky_models: DashMap<String, String>,
    pub in_flight: DashMap<String, AtomicUsize>,
    pub recent_errors: DashMap<String, AtomicUsize>,
}

impl ComboTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
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
        let entry = self
            .recent_errors
            .entry(candidate.to_string())
            .or_insert_with(|| AtomicUsize::new(0));
        entry.fetch_add(1, Ordering::SeqCst);
    }

    pub fn record_success(&self, combo_name: &str, candidate: &str) {
        // Clear or decrement recent errors on success
        if let Some(errs) = self.recent_errors.get(candidate) {
            errs.store(0, Ordering::SeqCst);
        }
        // Update sticky candidate for sticky routing
        self.sticky_models
            .insert(combo_name.to_string(), candidate.to_string());
    }

    /// Orders candidate models based on the combo strategy.
    pub fn order_candidates(
        &self,
        combo_name: &str,
        candidates: &[String],
        strategy: ComboStrategy,
    ) -> Vec<String> {
        if candidates.is_empty() {
            return Vec::new();
        }

        match strategy {
            ComboStrategy::Fallback => candidates.to_vec(),

            ComboStrategy::RoundRobin => {
                let counter = self
                    .rr_counters
                    .entry(combo_name.to_string())
                    .or_insert_with(|| AtomicUsize::new(0));
                let idx = counter.fetch_add(1, Ordering::SeqCst) % candidates.len();
                let mut reordered = Vec::with_capacity(candidates.len());
                reordered.extend_from_slice(&candidates[idx..]);
                reordered.extend_from_slice(&candidates[..idx]);
                reordered
            }

            ComboStrategy::Sticky => {
                if let Some(sticky) = self.sticky_models.get(combo_name) {
                    if let Some(pos) = candidates.iter().position(|c| c == sticky.value()) {
                        let mut reordered = Vec::with_capacity(candidates.len());
                        reordered.push(candidates[pos].clone());
                        for (i, c) in candidates.iter().enumerate() {
                            if i != pos {
                                reordered.push(c.clone());
                            }
                        }
                        return reordered;
                    }
                }
                candidates.to_vec()
            }

            ComboStrategy::Capacity => {
                // Capacity-aware: sort candidates by (in_flight_load * 1000 + recent_errors * 100)
                let mut scored: Vec<(usize, String)> = candidates
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

            ComboStrategy::Fusion => {
                // Fusion uses parallel speculative racing
                candidates.to_vec()
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
}
