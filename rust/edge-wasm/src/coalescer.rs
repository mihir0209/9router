use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::coordinator::{GatewayCoordinator, MetricItem, OutcomeItem};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoalescerConfig {
    pub coordinator_read_ttl_ms: u64,
    pub coordinator_flush_ms: u64,
    pub coordinator_flush_max_events: usize,
}

impl Default for CoalescerConfig {
    fn default() -> Self {
        Self {
            coordinator_read_ttl_ms: 5_000,
            coordinator_flush_ms: 30_000,
            coordinator_flush_max_events: 500,
        }
    }
}

/// In-memory traffic shaper and event batcher for WebAssembly edge runtime.
/// Shields Cloudflare Workers free tier CPU budget by eliminating 3-5 DO RPCs per request.
pub struct CoordinatorCoalescer {
    coordinator: Arc<GatewayCoordinator>,
    config: CoalescerConfig,
    cache: RwLock<HashMap<String, (Value, u64, String)>>, // key -> (value, expires_at_ms, scope)
    pending_outcomes: Mutex<HashMap<String, OutcomeItem>>,
    pending_metrics: Mutex<HashMap<String, MetricItem>>,
    pending_events: Mutex<usize>,
    first_event_at: Mutex<Option<u64>>,
}

impl CoordinatorCoalescer {
    pub fn new(coordinator: Arc<GatewayCoordinator>, config: CoalescerConfig) -> Self {
        Self {
            coordinator,
            config,
            cache: RwLock::new(HashMap::new()),
            pending_outcomes: Mutex::new(HashMap::new()),
            pending_metrics: Mutex::new(HashMap::new()),
            pending_events: Mutex::new(0),
            first_event_at: Mutex::new(None),
        }
    }

    pub fn get_cached(&self, key: &str, now: u64) -> Option<Value> {
        let guard = self.cache.read().unwrap();
        if let Some((val, expires_at, _)) = guard.get(key) {
            if *expires_at > now {
                return Some(val.clone());
            }
        }
        None
    }

    pub fn set_cached(&self, key: &str, scope: &str, value: Value, now: u64, ttl_ms: Option<u64>) {
        let ttl = ttl_ms.unwrap_or(self.config.coordinator_read_ttl_ms);
        let mut guard = self.cache.write().unwrap();
        guard.insert(
            key.to_string(),
            (value, now.saturating_add(ttl), scope.to_string()),
        );
    }

    pub fn read_cached<F>(&self, key: &str, scope: &str, now: u64, loader: F) -> Value
    where
        F: FnOnce() -> Value,
    {
        if self.config.coordinator_read_ttl_ms == 0 {
            return loader();
        }

        if let Some(cached) = self.get_cached(key, now) {
            return cached;
        }

        let loaded = loader();
        self.set_cached(key, scope, loaded.clone(), now, None);
        loaded
    }

    pub fn invalidate(&self, scope: &str) {
        let mut guard = self.cache.write().unwrap();
        guard.retain(|_, (_, _, s)| s != scope);
    }

    pub fn queue_outcome(
        &self,
        provider: &str,
        connection_id: &str,
        model: &str,
        success: bool,
        latency_ms: Option<f64>,
        status: u16,
        cost_usd: f64,
        now: u64,
    ) {
        if provider.is_empty() || connection_id.is_empty() {
            return;
        }

        let key = format!("{}|{}|{}", provider, connection_id, model);
        let mut outcomes = self.pending_outcomes.lock().unwrap();

        let item = outcomes.entry(key).or_insert_with(|| OutcomeItem {
            provider: provider.to_string(),
            connection_id: connection_id.to_string(),
            model: model.to_string(),
            samples: 0,
            successes: 0,
            failures: 0,
            tail_failures: 0,
            latency_sum_ms: 0.0,
            latency_count: 0,
            cost_usd: 0.0,
            last_status: 0,
        });

        item.samples += 1;
        if success {
            item.successes += 1;
            item.tail_failures = 0;
        } else {
            item.failures += 1;
            item.tail_failures += 1;
        }

        if let Some(lat) = latency_ms {
            let l = lat.max(0.0);
            item.latency_sum_ms += l;
            item.latency_count += 1;
        }

        item.cost_usd += cost_usd.max(0.0);
        item.last_status = status;
        drop(outcomes);

        self.note_event(now);
    }

    pub fn queue_metrics(
        &self,
        tenant: Option<&str>,
        provider: &str,
        connection_id: &str,
        model: &str,
        success: bool,
        status: u16,
        latency_ms: Option<f64>,
        now: u64,
    ) {
        if provider.is_empty() {
            return;
        }

        let t = tenant.unwrap_or("anonymous");
        let key = format!("{}|{}|{}|{}", t, provider, connection_id, model);
        let mut metrics = self.pending_metrics.lock().unwrap();

        let item = metrics.entry(key).or_insert_with(|| MetricItem {
            tenant: t.to_string(),
            provider: provider.to_string(),
            connection_id: connection_id.to_string(),
            model: model.to_string(),
            successes: 0,
            failures: 0,
            tail_failures: 0,
            last_status: 0,
            last_latency_ms: None,
            latency_count: 0,
            latency_sum_ms: 0.0,
            latency_max_ms: 0.0,
        });

        if success {
            item.successes += 1;
            item.tail_failures = 0;
        } else {
            item.failures += 1;
            item.tail_failures += 1;
        }

        item.last_status = status;
        if let Some(lat) = latency_ms {
            let l = lat.max(0.0);
            item.last_latency_ms = Some(l);
            item.latency_count += 1;
            item.latency_sum_ms += l;
            item.latency_max_ms = item.latency_max_ms.max(l);
        }
        drop(metrics);

        self.note_event(now);
    }

    fn note_event(&self, now: u64) {
        let mut count = self.pending_events.lock().unwrap();
        *count += 1;
        let mut first = self.first_event_at.lock().unwrap();
        if first.is_none() {
            *first = Some(now);
        }

        let should_flush = *count >= self.config.coordinator_flush_max_events
            || first
                .map(|t| now.saturating_sub(t) >= self.config.coordinator_flush_ms)
                .unwrap_or(false);

        if should_flush {
            drop(count);
            drop(first);
            self.flush(now);
        }
    }

    pub fn should_flush(&self, now: u64) -> bool {
        let count = *self.pending_events.lock().unwrap();
        let first = *self.first_event_at.lock().unwrap();
        count >= self.config.coordinator_flush_max_events
            || first
                .map(|t| now.saturating_sub(t) >= self.config.coordinator_flush_ms)
                .unwrap_or(false)
    }

    pub fn flush(&self, now: u64) -> usize {
        let outcomes_to_send: Vec<OutcomeItem> = {
            let mut out = self.pending_outcomes.lock().unwrap();
            let items = out.values().cloned().collect();
            out.clear();
            items
        };

        let metrics_to_send: Vec<MetricItem> = {
            let mut met = self.pending_metrics.lock().unwrap();
            let items = met.values().cloned().collect();
            met.clear();
            items
        };

        {
            let mut count = self.pending_events.lock().unwrap();
            *count = 0;
            let mut first = self.first_event_at.lock().unwrap();
            *first = None;
        }

        let mut total_sent = 0;
        if !outcomes_to_send.is_empty() {
            total_sent += self.coordinator.record_outcomes(&outcomes_to_send, now);
        }

        if !metrics_to_send.is_empty() {
            total_sent += self.coordinator.record_metrics_batch(&metrics_to_send, now);
        }

        total_sent
    }

    pub fn pending_counts(&self) -> (usize, usize, usize) {
        let events = *self.pending_events.lock().unwrap();
        let outcomes = self.pending_outcomes.lock().unwrap().len();
        let metrics = self.pending_metrics.lock().unwrap().len();
        (events, outcomes, metrics)
    }
}
