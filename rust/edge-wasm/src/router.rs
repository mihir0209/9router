use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use serde::{Deserialize, Serialize};

use crate::coordinator::GatewayCoordinator;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComboStrategy {
    Fallback,
    RoundRobin,
    Sticky,
    Capacity,
    HedgedRacing,
    Fusion,
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
            "hedged" | "race" | "hedged-racing" | "hedged_racing" | "racing" => Self::HedgedRacing,
            "fusion" => Self::Fusion,
            _ => Self::Fallback,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    pub id: String,
    pub key: String,
    pub rate_limit: Option<usize>,
    pub rate_limit_window_ms: Option<u64>,
    pub max_concurrent: Option<usize>,
    #[serde(default)]
    pub allowed_models: Vec<String>,
    #[serde(default = "default_true")]
    pub is_active: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComboConfig {
    pub name: String,
    #[serde(default = "default_strategy")]
    pub strategy: ComboStrategy,
    pub models: Vec<String>,
}

fn default_strategy() -> ComboStrategy {
    ComboStrategy::Fallback
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConnection {
    pub provider: String,
    pub connection_id: String,
    pub model: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_true")]
    pub is_active: bool,
    pub base_url: Option<String>,
}

fn default_weight() -> u32 {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastRouteRequest {
    pub model: String,
    pub api_key: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastRouteResponse {
    pub allowed: bool,
    pub status: u16,
    pub error_message: Option<String>,
    pub resolved_model: String,
    pub selected_provider: String,
    pub selected_connection_id: String,
    pub base_url: Option<String>,
    pub lease_id: Option<String>,
    pub rate_limit_remaining: usize,
    pub rate_limit_reset_at: u64,
    pub is_hedged: bool,
    pub backup_model: Option<String>,
    pub backup_provider: Option<String>,
    pub backup_connection_id: Option<String>,
}

/// Fast path route resolver for 9Router on Cloudflare Workers edge.
/// Evaluates API key validity, rate limit, concurrency lease, model alias, combo strategy,
/// and connection health within microseconds (<0.1ms).
pub struct FastRouteResolver {
    coordinator: Arc<GatewayCoordinator>,
    api_keys: RwLock<HashMap<String, ApiKeyRecord>>,     // key_string -> ApiKeyRecord
    aliases: RwLock<HashMap<String, String>>,            // alias -> target_model
    combos: RwLock<HashMap<String, ComboConfig>>,        // combo_name -> ComboConfig
    connections: RwLock<HashMap<String, Vec<ProviderConnection>>>, // model -> connections
    rr_counters: RwLock<HashMap<String, usize>>,         // combo_name -> round-robin index
    sticky_picks: RwLock<HashMap<String, String>>,       // combo_name:api_key -> chosen_model
}

impl FastRouteResolver {
    pub fn new(coordinator: Arc<GatewayCoordinator>) -> Self {
        Self {
            coordinator,
            api_keys: RwLock::new(HashMap::new()),
            aliases: RwLock::new(HashMap::new()),
            combos: RwLock::new(HashMap::new()),
            connections: RwLock::new(HashMap::new()),
            rr_counters: RwLock::new(HashMap::new()),
            sticky_picks: RwLock::new(HashMap::new()),
        }
    }

    pub fn set_api_keys(&self, keys: Vec<ApiKeyRecord>) {
        let mut guard = self.api_keys.write().unwrap();
        guard.clear();
        for k in keys {
            guard.insert(k.key.clone(), k);
        }
    }

    pub fn set_aliases(&self, aliases: HashMap<String, String>) {
        let mut guard = self.aliases.write().unwrap();
        *guard = aliases;
    }

    pub fn set_combos(&self, combos: Vec<ComboConfig>) {
        let mut guard = self.combos.write().unwrap();
        guard.clear();
        for c in combos {
            guard.insert(c.name.clone(), c);
        }
    }

    pub fn set_connections(&self, conns: Vec<ProviderConnection>) {
        let mut guard = self.connections.write().unwrap();
        guard.clear();
        for c in conns {
            let entry = guard.entry(c.model.clone()).or_default();
            entry.push(c);
        }
    }

    /// Resolves the fastest, healthiest upstream route for a client request.
    pub fn resolve(&self, req: &FastRouteRequest, now: u64) -> FastRouteResponse {
        let mut lease_id = None;
        let mut rate_limit_remaining = usize::MAX;
        let mut rate_limit_reset_at = 0;

        // 1. Authenticate & enforce rate limits / concurrency leases if key provided
        let key_record = if let Some(ref raw_key) = req.api_key {
            let guard = self.api_keys.read().unwrap();
            match guard.get(raw_key) {
                Some(k) if k.is_active => Some(k.clone()),
                Some(_) => {
                    return FastRouteResponse {
                        allowed: false,
                        status: 401,
                        error_message: Some("API key is inactive".to_string()),
                        resolved_model: req.model.clone(),
                        selected_provider: String::new(),
                        selected_connection_id: String::new(),
                        base_url: None,
                        lease_id: None,
                        rate_limit_remaining: 0,
                        rate_limit_reset_at: 0,
                        is_hedged: false,
                        backup_model: None,
                        backup_provider: None,
                        backup_connection_id: None,
                    };
                }
                None => {
                    return FastRouteResponse {
                        allowed: false,
                        status: 401,
                        error_message: Some("Invalid API key".to_string()),
                        resolved_model: req.model.clone(),
                        selected_provider: String::new(),
                        selected_connection_id: String::new(),
                        base_url: None,
                        lease_id: None,
                        rate_limit_remaining: 0,
                        rate_limit_reset_at: 0,
                        is_hedged: false,
                        backup_model: None,
                        backup_provider: None,
                        backup_connection_id: None,
                    };
                }
            }
        } else {
            None
        };

        if let Some(ref k) = key_record {
            // Check allowed models
            if !k.allowed_models.is_empty()
                && !k.allowed_models.contains(&"*".to_string())
                && !k.allowed_models.contains(&req.model)
            {
                return FastRouteResponse {
                    allowed: false,
                    status: 403,
                    error_message: Some(format!("Model '{}' not permitted for this key", req.model)),
                    resolved_model: req.model.clone(),
                    selected_provider: String::new(),
                    selected_connection_id: String::new(),
                    base_url: None,
                    lease_id: None,
                    rate_limit_remaining: 0,
                    rate_limit_reset_at: 0,
                    is_hedged: false,
                    backup_model: None,
                    backup_provider: None,
                    backup_connection_id: None,
                };
            }

            // Check rate limit
            let limit = k.rate_limit.unwrap_or(10_000);
            let window_ms = k.rate_limit_window_ms.unwrap_or(60_000);
            let rl = self.coordinator.check_rate_limit(&k.id, limit, window_ms, now);
            rate_limit_remaining = rl.remaining;
            rate_limit_reset_at = rl.reset_at;

            if !rl.allowed {
                return FastRouteResponse {
                    allowed: false,
                    status: 429,
                    error_message: Some("Rate limit exceeded".to_string()),
                    resolved_model: req.model.clone(),
                    selected_provider: String::new(),
                    selected_connection_id: String::new(),
                    base_url: None,
                    lease_id: None,
                    rate_limit_remaining: rl.remaining,
                    rate_limit_reset_at: rl.reset_at,
                    is_hedged: false,
                    backup_model: None,
                    backup_provider: None,
                    backup_connection_id: None,
                };
            }

            // Check concurrency lease
            let max_concurrent = k.max_concurrent.unwrap_or(100);
            let lease_res = self.coordinator.begin_request(&k.id, max_concurrent, now);
            if !lease_res.allowed {
                return FastRouteResponse {
                    allowed: false,
                    status: 429,
                    error_message: Some("Concurrency limit exceeded".to_string()),
                    resolved_model: req.model.clone(),
                    selected_provider: String::new(),
                    selected_connection_id: String::new(),
                    base_url: None,
                    lease_id: None,
                    rate_limit_remaining: rl.remaining,
                    rate_limit_reset_at: rl.reset_at,
                    is_hedged: false,
                    backup_model: None,
                    backup_provider: None,
                    backup_connection_id: None,
                };
            }
            lease_id = lease_res.lease_id;
        }

        // 2. Resolve Model Aliases
        let mut target_model = req.model.clone();
        {
            let aliases = self.aliases.read().unwrap();
            if let Some(aliased) = aliases.get(&target_model) {
                target_model = aliased.clone();
            }
        }

        // 3. Resolve Combos
        let mut is_hedged = false;
        let mut backup_model = None;

        let resolved_model = {
            let combos = self.combos.read().unwrap();
            if let Some(combo) = combos.get(&target_model) {
                let candidates = &combo.models;
                if candidates.is_empty() {
                    target_model
                } else {
                    // Check locks for each candidate's provider connections
                    let available_candidates: Vec<String> = candidates
                        .iter()
                        .filter(|c| {
                            let conns_guard = self.connections.read().unwrap();
                            if let Some(conns) = conns_guard.get(*c) {
                                for conn in conns {
                                    if !conn.is_active {
                                        continue;
                                    }
                                    let locks = self.coordinator.get_active_locks(
                                        &conn.provider,
                                        &[conn.connection_id.clone()],
                                        Some(c),
                                        now,
                                    );
                                    if locks.locks.is_empty() {
                                        return true; // At least one connection is unlocked
                                    }
                                }
                                false
                            } else {
                                true
                            }
                        })
                        .cloned()
                        .collect();

                    let effective = if !available_candidates.is_empty() {
                        available_candidates
                    } else {
                        candidates.clone() // Fallback to all if all locked
                    };

                    match combo.strategy {
                        ComboStrategy::Fallback => effective[0].clone(),

                        ComboStrategy::RoundRobin => {
                            let mut counters = self.rr_counters.write().unwrap();
                            let idx = counters.entry(combo.name.clone()).or_insert(0);
                            let chosen = effective[*idx % effective.len()].clone();
                            *idx = (*idx + 1) % effective.len();
                            chosen
                        }

                        ComboStrategy::Sticky => {
                            let key_id = key_record.as_ref().map(|k| k.id.as_str()).unwrap_or("anon");
                            let sticky_key = format!("{}:{}", combo.name, key_id);
                            let mut picks = self.sticky_picks.write().unwrap();
                            if let Some(prev) = picks.get(&sticky_key) {
                                if effective.contains(prev) {
                                    prev.clone()
                                } else {
                                    let chosen = effective[0].clone();
                                    picks.insert(sticky_key, chosen.clone());
                                    chosen
                                }
                            } else {
                                let chosen = effective[0].clone();
                                picks.insert(sticky_key, chosen.clone());
                                chosen
                            }
                        }

                        ComboStrategy::Capacity => {
                            // Select candidate with lowest EWMA latency
                            let mut best_candidate = effective[0].clone();
                            let mut best_latency = f64::MAX;

                            for cand in &effective {
                                let conns_guard = self.connections.read().unwrap();
                                if let Some(conns) = conns_guard.get(cand) {
                                    for conn in conns {
                                        let scores = self.coordinator.get_scores(
                                            &conn.provider,
                                            &[conn.connection_id.clone()],
                                            cand,
                                            now,
                                        );
                                        if let Some(sc) = scores.first() {
                                            if sc.ewma_latency_ms < best_latency {
                                                best_latency = sc.ewma_latency_ms;
                                                best_candidate = cand.clone();
                                            }
                                        }
                                    }
                                }
                            }
                            best_candidate
                        }

                        ComboStrategy::HedgedRacing | ComboStrategy::Fusion => {
                            is_hedged = true;
                            if effective.len() > 1 {
                                backup_model = Some(effective[1].clone());
                            }
                            effective[0].clone()
                        }
                    }
                }
            } else {
                target_model
            }
        };

        // 4. Select Provider Connection for resolved model
        let (selected_provider, selected_connection_id, base_url) = {
            let conns_guard = self.connections.read().unwrap();
            if let Some(conns) = conns_guard.get(&resolved_model) {
                // Filter active and unlocked connections
                let mut valid_conns = Vec::new();
                for c in conns {
                    if !c.is_active {
                        continue;
                    }
                    let locks = self.coordinator.get_active_locks(
                        &c.provider,
                        &[c.connection_id.clone()],
                        Some(&resolved_model),
                        now,
                    );
                    if locks.locks.is_empty() {
                        valid_conns.push(c.clone());
                    }
                }

                if valid_conns.is_empty() {
                    // Fall back to any active connection
                    valid_conns = conns.iter().filter(|c| c.is_active).cloned().collect();
                }

                if let Some(first) = valid_conns.first() {
                    (first.provider.clone(), first.connection_id.clone(), first.base_url.clone())
                } else {
                    ("direct".to_string(), "conn-0".to_string(), None)
                }
            } else {
                ("direct".to_string(), "conn-0".to_string(), None)
            }
        };

        // If hedged, also resolve backup provider connection
        let (backup_provider, backup_connection_id) = if let Some(ref bk) = backup_model {
            let conns_guard = self.connections.read().unwrap();
            if let Some(conns) = conns_guard.get(bk) {
                if let Some(c) = conns.iter().find(|c| c.is_active) {
                    (Some(c.provider.clone()), Some(c.connection_id.clone()))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        FastRouteResponse {
            allowed: true,
            status: 200,
            error_message: None,
            resolved_model,
            selected_provider,
            selected_connection_id,
            base_url,
            lease_id,
            rate_limit_remaining,
            rate_limit_reset_at,
            is_hedged,
            backup_model,
            backup_provider,
            backup_connection_id,
        }
    }
}
