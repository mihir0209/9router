use std::collections::HashMap;
use std::sync::RwLock;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitResult {
    pub allowed: bool,
    pub count: usize,
    pub remaining: usize,
    pub reset_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitPeek {
    pub count: usize,
    pub remaining: usize,
    pub reset_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLease {
    pub lease_id: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeginRequestResult {
    pub allowed: bool,
    pub active: usize,
    pub lease_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewRequestResult {
    pub renewed: bool,
    pub active: usize,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveLock {
    pub connection_id: String,
    pub until_ms: u64,
    pub status: u16,
    pub reason: String,
    pub generation: u64,
    pub set_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockResult {
    pub locked: bool,
    pub until_ms: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearLockResult {
    pub cleared: bool,
    pub reason: Option<String>,
    pub generation: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveLocksResponse {
    pub locks: Vec<ActiveLock>,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreEntry {
    pub provider: String,
    pub connection_id: String,
    pub model: String,
    pub samples: usize,
    pub successes: usize,
    pub failures: usize,
    pub consecutive_failures: usize,
    pub ewma_latency_ms: f64,
    pub total_cost_usd: f64,
    pub last_status: u16,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeItem {
    pub provider: String,
    pub connection_id: String,
    pub model: String,
    pub samples: usize,
    pub successes: usize,
    pub failures: usize,
    pub tail_failures: usize,
    pub latency_sum_ms: f64,
    pub latency_count: usize,
    pub cost_usd: f64,
    pub last_status: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricItem {
    #[serde(default)]
    pub tenant: String,
    pub provider: String,
    pub connection_id: String,
    pub model: String,
    pub successes: usize,
    pub failures: usize,
    pub tail_failures: usize,
    pub last_status: u16,
    pub last_latency_ms: Option<f64>,
    pub latency_count: usize,
    pub latency_sum_ms: f64,
    pub latency_max_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetReservation {
    pub request_id: String,
    pub amount: f64,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReserveBudgetResult {
    pub reserved: bool,
    pub amount: f64,
    pub request_id: Option<String>,
    pub reserved_amount: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseBudgetResult {
    pub released: bool,
    pub reservations: Vec<BudgetReservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsTotals {
    pub started_at: u64,
    pub requests: usize,
    pub successes: usize,
    pub failures: usize,
    pub latency_count: usize,
    pub latency_sum_ms: f64,
    pub latency_max_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantTotals {
    pub tenant: String,
    pub requests: usize,
    pub successes: usize,
    pub failures: usize,
}

#[derive(Debug, Default)]
struct RateWindow {
    count: usize,
    window_start: u64,
}

#[derive(Debug, Default)]
struct CoordinatorStorage {
    rates: HashMap<String, RateWindow>,
    active_leases: HashMap<String, Vec<RequestLease>>,
    lock_gens: HashMap<String, u64>,
    locks: HashMap<String, ActiveLock>,
    scores: HashMap<String, ScoreEntry>,
    budgets: HashMap<String, Vec<BudgetReservation>>,
    versions: HashMap<String, u64>,
    metric_totals: Option<MetricsTotals>,
    tenant_totals: HashMap<String, TenantTotals>,
    lease_counter: u64,
}

/// In-memory implementation of the GatewayCoordinator Durable Object contract bindings
/// compiled for WebAssembly edge execution.
pub struct GatewayCoordinator {
    storage: RwLock<CoordinatorStorage>,
}

impl GatewayCoordinator {
    pub fn new() -> Self {
        Self {
            storage: RwLock::new(CoordinatorStorage::default()),
        }
    }

    pub fn check_rate_limit(
        &self,
        key_id: &str,
        limit: usize,
        window_ms: u64,
        now: u64,
    ) -> RateLimitResult {
        let mut guard = self.storage.write().unwrap();
        let key = format!("rate:{}", key_id);
        let entry = guard.rates.entry(key).or_insert(RateWindow {
            count: 0,
            window_start: now,
        });

        if now.saturating_sub(entry.window_start) >= window_ms {
            entry.count = 0;
            entry.window_start = now;
        }

        entry.count += 1;
        let count = entry.count;
        let reset_at = entry.window_start + window_ms;
        let allowed = count <= limit;
        let remaining = if limit >= count { limit - count } else { 0 };

        RateLimitResult {
            allowed,
            count,
            remaining,
            reset_at,
        }
    }

    pub fn peek_rate_limit(
        &self,
        key_id: &str,
        limit: usize,
        window_ms: u64,
        now: u64,
    ) -> RateLimitPeek {
        let guard = self.storage.read().unwrap();
        let key = format!("rate:{}", key_id);

        if let Some(entry) = guard.rates.get(&key) {
            if now.saturating_sub(entry.window_start) < window_ms {
                let remaining = if limit >= entry.count {
                    limit - entry.count
                } else {
                    0
                };
                return RateLimitPeek {
                    count: entry.count,
                    remaining,
                    reset_at: entry.window_start + window_ms,
                };
            }
        }

        RateLimitPeek {
            count: 0,
            remaining: limit,
            reset_at: now + window_ms,
        }
    }

    pub fn begin_request(
        &self,
        key_id: &str,
        max_concurrent: usize,
        now: u64,
    ) -> BeginRequestResult {
        let mut guard = self.storage.write().unwrap();
        guard.lease_counter += 1;
        let counter = guard.lease_counter;

        let key = format!("active:{}", key_id);
        let active = guard.active_leases.entry(key).or_default();
        active.retain(|lease| lease.expires_at > now);

        if active.len() >= max_concurrent {
            return BeginRequestResult {
                allowed: false,
                active: active.len(),
                lease_id: None,
            };
        }

        let lease_id = format!("lease_{}_{}", now, counter);
        active.push(RequestLease {
            lease_id: lease_id.clone(),
            expires_at: now + 120_000,
        });

        BeginRequestResult {
            allowed: true,
            active: active.len(),
            lease_id: Some(lease_id),
        }
    }

    pub fn renew_request(
        &self,
        key_id: &str,
        lease_id: &str,
        ttl_ms: u64,
        now: u64,
    ) -> RenewRequestResult {
        let mut guard = self.storage.write().unwrap();
        let key = format!("active:{}", key_id);
        let active = guard.active_leases.entry(key).or_default();
        let live_count = active.iter().filter(|l| l.expires_at > now).count();

        for lease in active.iter_mut() {
            if lease.lease_id == lease_id {
                if lease.expires_at <= now {
                    return RenewRequestResult {
                        renewed: false,
                        active: live_count,
                        expires_at: lease.expires_at,
                    };
                }
                lease.expires_at = now + ttl_ms;
                return RenewRequestResult {
                    renewed: true,
                    active: live_count,
                    expires_at: lease.expires_at,
                };
            }
        }

        RenewRequestResult {
            renewed: false,
            active: live_count,
            expires_at: 0,
        }
    }

    pub fn end_request(&self, key_id: &str, lease_id: &str) -> usize {
        let mut guard = self.storage.write().unwrap();
        let key = format!("active:{}", key_id);
        if let Some(active) = guard.active_leases.get_mut(&key) {
            active.retain(|l| l.lease_id != lease_id);
            let len = active.len();
            if len == 0 {
                guard.active_leases.remove(&key);
            }
            return len;
        }
        0
    }

    pub fn set_lock(
        &self,
        provider: &str,
        connection_id: &str,
        model: Option<&str>,
        until_ms: u64,
        status: u16,
        reason: &str,
        now: u64,
    ) -> LockResult {
        let mut guard = self.storage.write().unwrap();
        let gen_key = format!("lockgen:{}", provider);
        let gen_counter = guard.lock_gens.entry(gen_key).or_insert(0);
        *gen_counter += 1;
        let current_gen = *gen_counter;

        let lock_key = match model {
            Some(m) if !m.is_empty() => format!("lock:{}:{}:{}", provider, connection_id, m),
            _ => format!("lock:{}:{}:*", provider, connection_id),
        };

        guard.locks.insert(
            lock_key,
            ActiveLock {
                connection_id: connection_id.to_string(),
                until_ms,
                status,
                reason: reason.to_string(),
                generation: current_gen,
                set_at: now,
            },
        );

        LockResult {
            locked: true,
            until_ms,
            generation: current_gen,
        }
    }

    pub fn clear_lock(
        &self,
        provider: &str,
        connection_id: &str,
        model: Option<&str>,
        expected_generation: Option<u64>,
    ) -> ClearLockResult {
        let mut guard = self.storage.write().unwrap();
        let lock_key = match model {
            Some(m) if !m.is_empty() => format!("lock:{}:{}:{}", provider, connection_id, m),
            _ => format!("lock:{}:{}:*", provider, connection_id),
        };

        if let Some(existing) = guard.locks.get(&lock_key) {
            if let Some(exp) = expected_generation {
                if existing.generation > exp {
                    return ClearLockResult {
                        cleared: false,
                        reason: Some("newer_lock".to_string()),
                        generation: Some(existing.generation),
                    };
                }
            }
            guard.locks.remove(&lock_key);
            return ClearLockResult {
                cleared: true,
                reason: None,
                generation: None,
            };
        }

        ClearLockResult {
            cleared: true,
            reason: None,
            generation: None,
        }
    }

    pub fn get_active_locks(
        &self,
        provider: &str,
        connection_ids: &[String],
        model: Option<&str>,
        now: u64,
    ) -> ActiveLocksResponse {
        let guard = self.storage.read().unwrap();
        let current_gen = guard
            .lock_gens
            .get(&format!("lockgen:{}", provider))
            .copied()
            .unwrap_or(0);

        let mut active = Vec::new();
        for cid in connection_ids {
            let keys = match model {
                Some(m) if !m.is_empty() => vec![
                    format!("lock:{}:{}:{}", provider, cid, m),
                    format!("lock:{}:{}:*", provider, cid),
                ],
                _ => vec![format!("lock:{}:{}:*", provider, cid)],
            };

            for k in keys {
                if let Some(lock) = guard.locks.get(&k) {
                    if lock.until_ms > now {
                        active.push(lock.clone());
                        break;
                    }
                }
            }
        }

        ActiveLocksResponse {
            locks: active,
            generation: current_gen,
        }
    }

    pub fn record_outcome(
        &self,
        provider: &str,
        connection_id: &str,
        model: &str,
        success: bool,
        latency_ms: Option<f64>,
        status: u16,
        cost_usd: f64,
        now: u64,
    ) -> ScoreEntry {
        let mut guard = self.storage.write().unwrap();
        let key = format!("score:{}:{}:{}", provider, connection_id, model);

        let previous = guard.scores.get(&key).cloned();
        let prior_samples = previous.as_ref().map(|p| p.samples).unwrap_or(0);
        let prior_latency = previous.as_ref().map(|p| p.ewma_latency_ms).unwrap_or(0.0);
        let prior_failures = previous
            .as_ref()
            .map(|p| p.consecutive_failures)
            .unwrap_or(0);
        let prior_cost = previous.as_ref().map(|p| p.total_cost_usd).unwrap_or(0.0);

        let lat = latency_ms.unwrap_or(prior_latency).max(0.0);
        let ewma = if prior_samples == 0 {
            lat
        } else {
            prior_latency * 0.7 + lat * 0.3
        };

        let entry = ScoreEntry {
            provider: provider.to_string(),
            connection_id: connection_id.to_string(),
            model: model.to_string(),
            samples: prior_samples + 1,
            successes: previous.as_ref().map(|p| p.successes).unwrap_or(0)
                + if success { 1 } else { 0 },
            failures: previous.as_ref().map(|p| p.failures).unwrap_or(0)
                + if success { 0 } else { 1 },
            consecutive_failures: if success { 0 } else { prior_failures + 1 },
            ewma_latency_ms: ewma,
            total_cost_usd: prior_cost + cost_usd.max(0.0),
            last_status: status,
            updated_at: now,
        };

        guard.scores.insert(key, entry.clone());
        entry
    }

    pub fn record_outcomes(&self, items: &[OutcomeItem], now: u64) -> usize {
        let mut guard = self.storage.write().unwrap();
        let mut applied = 0;

        for item in items {
            if item.provider.is_empty() || item.connection_id.is_empty() || item.samples == 0 {
                continue;
            }
            let key = format!(
                "score:{}:{}:{}",
                item.provider, item.connection_id, item.model
            );

            let previous = guard.scores.get(&key).cloned();
            let prior_samples = previous.as_ref().map(|p| p.samples).unwrap_or(0);
            let prior_latency = previous.as_ref().map(|p| p.ewma_latency_ms).unwrap_or(0.0);

            let mean_latency = if item.latency_count > 0 {
                item.latency_sum_ms.max(0.0) / (item.latency_count as f64)
            } else {
                prior_latency
            };

            // Decay EWMA over n batch events: decay = 0.7^latency_count
            let mut decay = 1.0;
            for _ in 0..item.latency_count {
                decay *= 0.7;
            }
            let ewma = if prior_samples == 0 {
                mean_latency
            } else {
                prior_latency * decay + mean_latency * (1.0 - decay)
            };

            let entry = ScoreEntry {
                provider: item.provider.clone(),
                connection_id: item.connection_id.clone(),
                model: item.model.clone(),
                samples: prior_samples + item.samples,
                successes: previous.as_ref().map(|p| p.successes).unwrap_or(0) + item.successes,
                failures: previous.as_ref().map(|p| p.failures).unwrap_or(0) + item.failures,
                consecutive_failures: if item.successes == 0 {
                    previous
                        .as_ref()
                        .map(|p| p.consecutive_failures)
                        .unwrap_or(0)
                        + item.failures
                } else {
                    item.tail_failures
                },
                ewma_latency_ms: ewma,
                total_cost_usd: previous.as_ref().map(|p| p.total_cost_usd).unwrap_or(0.0)
                    + item.cost_usd.max(0.0),
                last_status: item.last_status,
                updated_at: now,
            };

            guard.scores.insert(key, entry);
            applied += 1;
        }

        applied
    }

    pub fn get_scores(
        &self,
        provider: &str,
        connection_ids: &[String],
        model: &str,
        now: u64,
    ) -> Vec<ScoreEntry> {
        let mut guard = self.storage.write().unwrap();
        let mut res = Vec::new();

        for cid in connection_ids {
            let key = format!("score:{}:{}:{}", provider, cid, model);
            if let Some(entry) = guard.scores.get(&key) {
                // Expire scores older than 24h (86,400,000 ms)
                if now.saturating_sub(entry.updated_at) > 86_400_000 {
                    guard.scores.remove(&key);
                } else {
                    res.push(entry.clone());
                }
            }
        }

        res
    }

    pub fn reserve_budget(
        &self,
        key_id: &str,
        request_id: &str,
        amount: f64,
        expires_at: u64,
        available_budget: Option<f64>,
        now: u64,
    ) -> ReserveBudgetResult {
        let mut guard = self.storage.write().unwrap();
        let key = format!("budget:{}", key_id);
        let reservations = guard.budgets.entry(key).or_default();
        reservations.retain(|r| r.expires_at > now);

        if let Some(existing) = reservations.iter().find(|r| r.request_id == request_id) {
            return ReserveBudgetResult {
                reserved: true,
                amount: existing.amount,
                request_id: Some(request_id.to_string()),
                reserved_amount: None,
            };
        }

        let reserved_sum: f64 = reservations.iter().map(|r| r.amount).sum();
        if let Some(avail) = available_budget {
            if reserved_sum + amount > avail {
                return ReserveBudgetResult {
                    reserved: false,
                    amount: 0.0,
                    request_id: None,
                    reserved_amount: Some(reserved_sum),
                };
            }
        }

        reservations.push(BudgetReservation {
            request_id: request_id.to_string(),
            amount,
            expires_at,
        });

        ReserveBudgetResult {
            reserved: true,
            amount,
            request_id: Some(request_id.to_string()),
            reserved_amount: Some(reserved_sum + amount),
        }
    }

    pub fn release_budget(&self, key_id: &str, request_id: &str) -> ReleaseBudgetResult {
        let mut guard = self.storage.write().unwrap();
        let key = format!("budget:{}", key_id);
        if let Some(reservations) = guard.budgets.get_mut(&key) {
            reservations.retain(|r| r.request_id != request_id);
            let copy = reservations.clone();
            if copy.is_empty() {
                guard.budgets.remove(&key);
            }
            return ReleaseBudgetResult {
                released: true,
                reservations: copy,
            };
        }

        ReleaseBudgetResult {
            released: false,
            reservations: Vec::new(),
        }
    }

    pub fn get_budget_reservations(&self, key_id: &str, now: u64) -> Vec<BudgetReservation> {
        let guard = self.storage.read().unwrap();
        let key = format!("budget:{}", key_id);
        guard
            .budgets
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.expires_at > now)
            .collect()
    }

    pub fn bump_version(&self, domain: &str) -> u64 {
        let mut guard = self.storage.write().unwrap();
        let next = guard.versions.entry(domain.to_string()).or_insert(0);
        *next += 1;
        *next
    }

    pub fn get_versions(&self, domains: &[String]) -> HashMap<String, u64> {
        let guard = self.storage.read().unwrap();
        let mut map = HashMap::new();
        for d in domains {
            map.insert(d.clone(), guard.versions.get(d).copied().unwrap_or(0));
        }
        map
    }

    pub fn record_metrics_batch(&self, items: &[MetricItem], now: u64) -> usize {
        let mut guard = self.storage.write().unwrap();
        let valid: Vec<_> = items
            .iter()
            .filter(|it| !it.provider.is_empty() && (it.successes + it.failures > 0))
            .collect();

        if valid.is_empty() {
            return 0;
        }

        let mut totals = guard.metric_totals.clone().unwrap_or(MetricsTotals {
            started_at: now,
            requests: 0,
            successes: 0,
            failures: 0,
            latency_count: 0,
            latency_sum_ms: 0.0,
            latency_max_ms: 0.0,
        });

        for item in &valid {
            let tenant = if item.tenant.trim().is_empty() {
                "anonymous"
            } else {
                item.tenant.trim()
            };

            let delta_reqs = item.successes + item.failures;
            totals.requests += delta_reqs;
            totals.successes += item.successes;
            totals.failures += item.failures;

            if item.latency_count > 0 {
                totals.latency_count += item.latency_count;
                totals.latency_sum_ms += item.latency_sum_ms.max(0.0);
                totals.latency_max_ms = totals.latency_max_ms.max(item.latency_max_ms);
            }

            let entry = guard
                .tenant_totals
                .entry(tenant.to_string())
                .or_insert_with(|| TenantTotals {
                    tenant: tenant.to_string(),
                    requests: 0,
                    successes: 0,
                    failures: 0,
                });
            entry.requests += delta_reqs;
            entry.successes += item.successes;
            entry.failures += item.failures;
        }

        guard.metric_totals = Some(totals);
        valid.len()
    }
}
