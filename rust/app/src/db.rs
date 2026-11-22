use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{error, warn};

use crate::config::Config;

/// Parsed data payload for provider connections.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionData {
    #[serde(rename = "apiKey", default)]
    pub api_key: Option<String>,
    #[serde(rename = "baseUrl", default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(rename = "defaultModel", default)]
    pub default_model: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Provider connection row with parsed credentials and settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConnection {
    pub id: String,
    pub provider: String,
    pub auth_type: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub priority: Option<i64>,
    pub is_active: bool,
    pub data: ConnectionData,
    pub raw_data: String,
    pub created_at: String,
    pub updated_at: String,
}

/// API Key row with access control and rate limiting rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub key: String,
    pub name: Option<String>,
    pub machine_id: Option<String>,
    pub is_active: bool,
    pub created_at: String,
    pub rate_limit_requests: i64,
    pub rate_limit_window_seconds: i64,
    pub token_limit_daily: i64,
    pub token_limit_monthly: i64,
    pub allowed_models: String,
    pub blocked_models: String,
    pub allowed_ips: String,
    pub blocked_ips: String,
    pub expires_at: String,
    pub description: String,
    pub owner_ref: Option<String>,
    pub max_concurrent_requests: i64,
    pub allowed_providers: String,
    pub blocked_providers: String,
    pub force_provider: String,
    pub force_model: String,
    pub allowed_endpoints: String,
    pub blocked_endpoints: String,
}

/// Combo model group with fallback list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Combo {
    pub id: String,
    pub name: String,
    pub kind: Option<String>,
    pub models: Vec<String>,
    pub strategy: Option<String>,
    pub match_rules: Option<String>,
    pub custom_prompt: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Provider node (custom endpoint with URL prefix routing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: Option<String>,
    pub name: Option<String>,
    pub prefix: String,
    #[serde(rename = "baseUrl")]
    pub base_url: String,
    #[serde(rename = "apiType")]
    pub api_type: Option<String>,
    pub data: serde_json::Value,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

/// Proxy pool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyPool {
    pub id: String,
    #[serde(rename = "isActive")]
    pub is_active: bool,
    #[serde(rename = "testStatus")]
    pub test_status: Option<String>,
    pub data: serde_json::Value,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

/// Plan definition for key budget and concurrency constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub description: Option<String>,
    pub limit_micros: i64,
    pub limit_tokens: i64,
    pub rpm_limit: i64,
    pub tpm_limit: i64,
    pub concurrency_limit: i64,
    pub period: String,
    pub alert_pct: i64,
    pub hard_cutoff: bool,
    pub allowed_models: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Usage record queued via write-behind logging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub timestamp: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub connection_id: Option<String>,
    pub api_key: Option<String>,
    pub api_key_id: Option<String>,
    pub endpoint: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost: f64,
    pub status: String,
    pub tokens: Option<String>,
    pub meta: Option<String>,
}

impl Default for UsageRecord {
    fn default() -> Self {
        Self {
            timestamp: Utc::now().to_rfc3339(),
            provider: None,
            model: None,
            connection_id: None,
            api_key: None,
            api_key_id: None,
            endpoint: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            cost: 0.0,
            status: "ok".to_string(),
            tokens: None,
            meta: None,
        }
    }
}

/// Auth errors returned during key validation and restriction enforcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyValidationError {
    NotFound,
    Disabled,
    Expired,
    RateLimited { limit: i64, window_seconds: i64 },
    ModelBlocked(String),
    ModelNotAllowed(String),
}

impl std::fmt::Display for KeyValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "Invalid API key"),
            Self::Disabled => write!(f, "API key is disabled"),
            Self::Expired => write!(f, "API key has expired"),
            Self::RateLimited {
                limit,
                window_seconds,
            } => {
                write!(
                    f,
                    "Rate limit exceeded: max {} requests per {}s",
                    limit, window_seconds
                )
            }
            Self::ModelBlocked(model) => {
                write!(f, "Model '{}' is blocked for this API key", model)
            }
            Self::ModelNotAllowed(model) => {
                write!(f, "Model '{}' is not allowed for this API key", model)
            }
        }
    }
}

impl std::error::Error for KeyValidationError {}

/// Shared SQLite database handle and background write-behind coordinator.
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    usage_tx: mpsc::Sender<UsageRecord>,
    rate_limiter: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
}

impl Database {
    /// Opens the SQLite database configured in the config object.
    pub fn open(config: &Config) -> anyhow::Result<Arc<Self>> {
        config.ensure_dirs()?;
        Self::open_path(&config.db_path)
    }

    /// Opens an in-memory database instance (primarily for tests).
    pub fn open_in_memory() -> anyhow::Result<Arc<Self>> {
        let conn = Connection::open_in_memory()?;
        Self::init_connection(conn)
    }

    /// Opens a database file at the given filesystem path.
    pub fn open_path(path: &Path) -> anyhow::Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::init_connection(conn)
    }

    fn init_connection(conn: Connection) -> anyhow::Result<Arc<Self>> {
        // Configure SQLite PRAGMAs: WAL mode, busy timeout 5000ms, synchronous NORMAL
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA busy_timeout = 5000;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            "#,
        )?;

        // Initialize schema tables if they do not exist
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS _meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS settings (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                data TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS providerConnections (
                id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                authType TEXT NOT NULL,
                name TEXT,
                email TEXT,
                priority INTEGER,
                isActive INTEGER DEFAULT 1,
                data TEXT NOT NULL,
                createdAt TEXT NOT NULL,
                updatedAt TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_pc_provider ON providerConnections(provider);
            CREATE INDEX IF NOT EXISTS idx_pc_provider_active ON providerConnections(provider, isActive);

            CREATE TABLE IF NOT EXISTS apiKeys (
                id TEXT PRIMARY KEY,
                key TEXT UNIQUE NOT NULL,
                name TEXT,
                machineId TEXT,
                isActive INTEGER DEFAULT 1,
                createdAt TEXT NOT NULL,
                rateLimitRequests INTEGER DEFAULT 0,
                rateLimitWindowSeconds INTEGER DEFAULT 60,
                tokenLimitDaily INTEGER DEFAULT 0,
                tokenLimitMonthly INTEGER DEFAULT 0,
                allowedModels TEXT DEFAULT '',
                blockedModels TEXT DEFAULT '',
                allowedIPs TEXT DEFAULT '',
                blockedIPs TEXT DEFAULT '',
                expiresAt TEXT DEFAULT '',
                description TEXT DEFAULT '',
                ownerRef TEXT,
                maxConcurrentRequests INTEGER DEFAULT 0,
                allowedProviders TEXT DEFAULT '',
                blockedProviders TEXT DEFAULT '',
                forceProvider TEXT DEFAULT '',
                forceModel TEXT DEFAULT '',
                allowedEndpoints TEXT DEFAULT '',
                blockedEndpoints TEXT DEFAULT '',
                metadata TEXT DEFAULT '{}',
                updatedAt TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_ak_key ON apiKeys(key);

            CREATE TABLE IF NOT EXISTS combos (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                kind TEXT,
                models TEXT NOT NULL,
                strategy TEXT DEFAULT 'fallback',
                matchRules TEXT DEFAULT '[]',
                customPrompt TEXT DEFAULT '',
                createdAt TEXT NOT NULL,
                updatedAt TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_combo_name ON combos(name);

            CREATE TABLE IF NOT EXISTS providerNodes (
                id TEXT PRIMARY KEY,
                type TEXT,
                name TEXT,
                data TEXT NOT NULL,
                createdAt TEXT NOT NULL,
                updatedAt TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS proxyPools (
                id TEXT PRIMARY KEY,
                isActive INTEGER DEFAULT 1,
                testStatus TEXT,
                data TEXT NOT NULL,
                createdAt TEXT NOT NULL,
                updatedAt TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS kv (
                scope TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (scope, key)
            );
            CREATE INDEX IF NOT EXISTS idx_kv_scope ON kv(scope);

            CREATE TABLE IF NOT EXISTS usageHistory (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                provider TEXT,
                model TEXT,
                connectionId TEXT,
                apiKey TEXT,
                apiKeyId TEXT,
                endpoint TEXT,
                promptTokens INTEGER DEFAULT 0,
                completionTokens INTEGER DEFAULT 0,
                cost REAL DEFAULT 0,
                status TEXT,
                tokens TEXT,
                meta TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_uh_ts ON usageHistory(timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_uh_provider ON usageHistory(provider);
            CREATE INDEX IF NOT EXISTS idx_uh_model ON usageHistory(model);
            CREATE INDEX IF NOT EXISTS idx_uh_conn ON usageHistory(connectionId);
            CREATE INDEX IF NOT EXISTS idx_uh_apikey ON usageHistory(apiKey);
            CREATE INDEX IF NOT EXISTS idx_uh_apikey_id ON usageHistory(apiKeyId);
            "#,
        )?;

        // Idempotent column migrations for existing databases
        let _ = conn.execute("ALTER TABLE combos ADD COLUMN strategy TEXT DEFAULT 'fallback'", []);
        let _ = conn.execute("ALTER TABLE combos ADD COLUMN matchRules TEXT DEFAULT '[]'", []);
        let _ = conn.execute("ALTER TABLE combos ADD COLUMN customPrompt TEXT DEFAULT ''", []);

        let (usage_tx, usage_rx) = mpsc::channel(10_000);
        let conn_arc = Arc::new(Mutex::new(conn));
        let rate_limiter = Arc::new(Mutex::new(HashMap::new()));

        let db = Arc::new(Self {
            conn: Arc::clone(&conn_arc),
            usage_tx,
            rate_limiter,
        });

        // Spawn background worker for batching usage history writes
        Self::spawn_usage_worker(Arc::clone(&conn_arc), usage_rx);

        Ok(db)
    }

    /// Spawns the write-behind background task for batch inserting usage records.
    fn spawn_usage_worker(
        conn: Arc<Mutex<Connection>>,
        mut rx: mpsc::Receiver<UsageRecord>,
    ) {
        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(64);
            let mut interval = tokio::time::interval(Duration::from_millis(500));

            loop {
                tokio::select! {
                    maybe_item = rx.recv() => {
                        match maybe_item {
                            Some(item) => {
                                batch.push(item);
                                if batch.len() >= 50 {
                                    Self::flush_usage_batch(&conn, &mut batch);
                                }
                            }
                            None => {
                                // Channel closed, flush remaining items and exit
                                if !batch.is_empty() {
                                    Self::flush_usage_batch(&conn, &mut batch);
                                }
                                break;
                            }
                        }
                    }
                    _ = interval.tick() => {
                        if !batch.is_empty() {
                            Self::flush_usage_batch(&conn, &mut batch);
                        }
                    }
                }
            }
        });
    }

    fn flush_usage_batch(conn: &Arc<Mutex<Connection>>, batch: &mut Vec<UsageRecord>) {
        if batch.is_empty() {
            return;
        }

        let guard = match conn.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };

        let result: rusqlite::Result<()> = (|| {
            let tx = guard.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare_cached(
                    r#"
                    INSERT INTO usageHistory(
                        timestamp, provider, model, connectionId, apiKey, apiKeyId,
                        endpoint, promptTokens, completionTokens, cost, status, tokens, meta
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                )?;

                for item in batch.drain(..) {
                    stmt.execute(params![
                        item.timestamp,
                        item.provider,
                        item.model,
                        item.connection_id,
                        item.api_key,
                        item.api_key_id,
                        item.endpoint,
                        item.prompt_tokens,
                        item.completion_tokens,
                        item.cost,
                        item.status,
                        item.tokens,
                        item.meta,
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })();

        if let Err(e) = result {
            error!("Failed to flush usageHistory batch: {}", e);
        }
    }

    /// Enqueue a usage record into the non-blocking write-behind channel.
    pub fn queue_usage(&self, record: UsageRecord) {
        if let Err(e) = self.usage_tx.try_send(record) {
            warn!("Write-behind queue is full or closed: {}", e);
        }
    }

    // ==========================================
    // API Keys Operations
    // ==========================================

    /// Look up an API key by exact key string.
    pub fn get_api_key_by_key(&self, key: &str) -> anyhow::Result<Option<ApiKey>> {
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare_cached(
            r#"
            SELECT id, key, name, machineId, isActive, createdAt,
                   rateLimitRequests, rateLimitWindowSeconds, tokenLimitDaily, tokenLimitMonthly,
                   allowedModels, blockedModels, allowedIPs, blockedIPs, expiresAt, description,
                   ownerRef, maxConcurrentRequests, allowedProviders, blockedProviders,
                   forceProvider, forceModel, allowedEndpoints, blockedEndpoints
            FROM apiKeys WHERE key = ?
            "#,
        )?;

        let result = stmt
            .query_row(params![key], |row| {
                Ok(ApiKey {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    name: row.get(2)?,
                    machine_id: row.get(3)?,
                    is_active: row.get::<_, i64>(4)? == 1,
                    created_at: row.get(5)?,
                    rate_limit_requests: row.get(6)?,
                    rate_limit_window_seconds: row.get(7)?,
                    token_limit_daily: row.get(8)?,
                    token_limit_monthly: row.get(9)?,
                    allowed_models: row.get(10)?,
                    blocked_models: row.get(11)?,
                    allowed_ips: row.get(12)?,
                    blocked_ips: row.get(13)?,
                    expires_at: row.get(14)?,
                    description: row.get(15)?,
                    owner_ref: row.get(16)?,
                    max_concurrent_requests: row.get(17)?,
                    allowed_providers: row.get(18)?,
                    blocked_providers: row.get(19)?,
                    force_provider: row.get(20)?,
                    force_model: row.get(21)?,
                    allowed_endpoints: row.get(22)?,
                    blocked_endpoints: row.get(23)?,
                })
            })
            .optional()?;

        Ok(result)
    }

    /// Insert or replace an API key record.
    pub fn save_api_key(&self, key: &ApiKey) -> anyhow::Result<()> {
        let guard = self.conn.lock().unwrap();
        guard.execute(
            r#"
            INSERT INTO apiKeys(
                id, key, name, machineId, isActive, createdAt,
                rateLimitRequests, rateLimitWindowSeconds, tokenLimitDaily, tokenLimitMonthly,
                allowedModels, blockedModels, allowedIPs, blockedIPs, expiresAt, description,
                ownerRef, maxConcurrentRequests, allowedProviders, blockedProviders,
                forceProvider, forceModel, allowedEndpoints, blockedEndpoints
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                key=excluded.key, name=excluded.name, machineId=excluded.machineId,
                isActive=excluded.isActive, rateLimitRequests=excluded.rateLimitRequests,
                rateLimitWindowSeconds=excluded.rateLimitWindowSeconds,
                allowedModels=excluded.allowedModels, blockedModels=excluded.blockedModels,
                expiresAt=excluded.expiresAt
            "#,
            params![
                key.id,
                key.key,
                key.name,
                key.machine_id,
                if key.is_active { 1 } else { 0 },
                key.created_at,
                key.rate_limit_requests,
                key.rate_limit_window_seconds,
                key.token_limit_daily,
                key.token_limit_monthly,
                key.allowed_models,
                key.blocked_models,
                key.allowed_ips,
                key.blocked_ips,
                key.expires_at,
                key.description,
                key.owner_ref,
                key.max_concurrent_requests,
                key.allowed_providers,
                key.blocked_providers,
                key.force_provider,
                key.force_model,
                key.allowed_endpoints,
                key.blocked_endpoints,
            ],
        )?;
        Ok(())
    }

    /// List all API keys.
    pub fn list_api_keys(&self) -> anyhow::Result<Vec<ApiKey>> {
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare(
            r#"
            SELECT id, key, name, machineId, isActive, createdAt,
                   rateLimitRequests, rateLimitWindowSeconds, tokenLimitDaily, tokenLimitMonthly,
                   allowedModels, blockedModels, allowedIPs, blockedIPs, expiresAt, description,
                   ownerRef, maxConcurrentRequests, allowedProviders, blockedProviders,
                   forceProvider, forceModel, allowedEndpoints, blockedEndpoints
            FROM apiKeys ORDER BY createdAt ASC
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(ApiKey {
                id: row.get(0)?,
                key: row.get(1)?,
                name: row.get(2)?,
                machine_id: row.get(3)?,
                is_active: row.get::<_, i64>(4)? == 1,
                created_at: row.get(5)?,
                rate_limit_requests: row.get(6)?,
                rate_limit_window_seconds: row.get(7)?,
                token_limit_daily: row.get(8)?,
                token_limit_monthly: row.get(9)?,
                allowed_models: row.get(10)?,
                blocked_models: row.get(11)?,
                allowed_ips: row.get(12)?,
                blocked_ips: row.get(13)?,
                expires_at: row.get(14)?,
                description: row.get(15)?,
                owner_ref: row.get(16)?,
                max_concurrent_requests: row.get(17)?,
                allowed_providers: row.get(18)?,
                blocked_providers: row.get(19)?,
                force_provider: row.get(20)?,
                force_model: row.get(21)?,
                allowed_endpoints: row.get(22)?,
                blocked_endpoints: row.get(23)?,
            })
        })?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    /// Delete an API key by ID or Key string.
    pub fn delete_api_key(&self, id: &str) -> anyhow::Result<bool> {
        let guard = self.conn.lock().unwrap();
        let affected = guard.execute(
            "DELETE FROM apiKeys WHERE id = ?1 OR key = ?1",
            params![id],
        )?;
        Ok(affected > 0)
    }

    /// Validates an API key: checks active flag, expiration date, and rate limits.
    pub fn validate_key(&self, api_key: &ApiKey) -> Result<(), KeyValidationError> {
        if !api_key.is_active {
            return Err(KeyValidationError::Disabled);
        }

        if !api_key.expires_at.trim().is_empty() {
            if let Ok(exp_time) = chrono::DateTime::parse_from_rfc3339(&api_key.expires_at) {
                if exp_time < Utc::now() {
                    return Err(KeyValidationError::Expired);
                }
            }
        }

        // Rate limit verification
        if api_key.rate_limit_requests > 0 {
            let mut limiter = self.rate_limiter.lock().unwrap();
            let now = Instant::now();
            let window = Duration::from_secs(api_key.rate_limit_window_seconds.max(1) as u64);
            let timestamps = limiter.entry(api_key.id.clone()).or_default();

            // Prune timestamps older than window
            timestamps.retain(|&t| now.duration_since(t) < window);

            if timestamps.len() >= api_key.rate_limit_requests as usize {
                return Err(KeyValidationError::RateLimited {
                    limit: api_key.rate_limit_requests,
                    window_seconds: api_key.rate_limit_window_seconds,
                });
            }

            timestamps.push(now);
        }

        Ok(())
    }

    /// Verifies if a given model is allowed by the API key's model policy.
    pub fn check_model_allowed(
        &self,
        api_key: &ApiKey,
        model: &str,
    ) -> Result<(), KeyValidationError> {
        let trimmed_model = model.trim();

        // If force_model is configured and matches, it is allowed
        if !api_key.force_model.trim().is_empty() && api_key.force_model.trim() == trimmed_model {
            return Ok(());
        }

        // Check blocked models list
        if !api_key.blocked_models.trim().is_empty() {
            let blocked_patterns: Vec<&str> = api_key
                .blocked_models
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();

            for pat in blocked_patterns {
                if pattern_matches(pat, trimmed_model) {
                    return Err(KeyValidationError::ModelBlocked(trimmed_model.to_string()));
                }
            }
        }

        // Check allowed models list
        if !api_key.allowed_models.trim().is_empty() {
            let allowed_patterns: Vec<&str> = api_key
                .allowed_models
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();

            if !allowed_patterns.is_empty() && !allowed_patterns.contains(&"*") {
                let any_match = allowed_patterns
                    .iter()
                    .any(|pat| pattern_matches(pat, trimmed_model));
                if !any_match {
                    return Err(KeyValidationError::ModelNotAllowed(
                        trimmed_model.to_string(),
                    ));
                }
            }
        }

        Ok(())
    }

    // ==========================================
    // Provider Connections Operations
    // ==========================================

    /// Loads all provider connections from the database.
    pub fn load_provider_connections(&self) -> anyhow::Result<Vec<ProviderConnection>> {
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare(
            r#"
            SELECT id, provider, authType, name, email, priority, isActive, data, createdAt, updatedAt
            FROM providerConnections ORDER BY priority ASC, createdAt ASC
            "#,
        )?;

        let rows = stmt.query_map([], |row| {
            let data_str: String = row.get(7)?;
            let parsed_data: ConnectionData =
                serde_json::from_str(&data_str).unwrap_or_default();

            Ok(ProviderConnection {
                id: row.get(0)?,
                provider: row.get(1)?,
                auth_type: row.get(2)?,
                name: row.get(3)?,
                email: row.get(4)?,
                priority: row.get(5)?,
                is_active: row.get::<_, i64>(6)? == 1,
                data: parsed_data,
                raw_data: data_str,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    /// Loads active connections, optionally filtered by provider.
    pub fn get_active_provider_connections(
        &self,
        provider: Option<&str>,
    ) -> anyhow::Result<Vec<ProviderConnection>> {
        let all = self.load_provider_connections()?;
        Ok(all
            .into_iter()
            .filter(|c| c.is_active && provider.map_or(true, |p| c.provider.eq_ignore_ascii_case(p)))
            .collect())
    }

    /// Saves or updates a provider connection.
    pub fn save_provider_connection(&self, conn: &ProviderConnection) -> anyhow::Result<()> {
        let guard = self.conn.lock().unwrap();
        let data_json = if conn.raw_data.trim().is_empty() {
            serde_json::to_string(&conn.data)?
        } else {
            conn.raw_data.clone()
        };

        guard.execute(
            r#"
            INSERT INTO providerConnections(
                id, provider, authType, name, email, priority, isActive, data, createdAt, updatedAt
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                provider=excluded.provider, authType=excluded.authType, name=excluded.name,
                email=excluded.email, priority=excluded.priority, isActive=excluded.isActive,
                data=excluded.data, updatedAt=excluded.updatedAt
            "#,
            params![
                conn.id,
                conn.provider,
                conn.auth_type,
                conn.name,
                conn.email,
                conn.priority,
                if conn.is_active { 1 } else { 0 },
                data_json,
                conn.created_at,
                conn.updated_at,
            ],
        )?;
        Ok(())
    }

    // ==========================================
    // Settings Operations
    // ==========================================

    /// Loads settings JSON from the database. Returns defaults if unset.
    pub fn load_settings(&self) -> anyhow::Result<serde_json::Value> {
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare_cached("SELECT data FROM settings WHERE id = 1")?;

        let row: Option<String> = stmt.query_row([], |r| r.get(0)).optional()?;
        if let Some(data) = row {
            if let Ok(v) = serde_json::from_str(&data) {
                return Ok(v);
            }
        }

        // Return default settings
        Ok(serde_json::json!({
            "requireLogin": true,
            "requireApiKey": true,
            "lowWriteMode": false,
            "tunnelEnabled": false,
            "cloudEnabled": false
        }))
    }

    /// Saves settings JSON into the database.
    pub fn save_settings(&self, settings: &serde_json::Value) -> anyhow::Result<()> {
        let guard = self.conn.lock().unwrap();
        let data_str = serde_json::to_string(settings)?;
        guard.execute(
            r#"
            INSERT INTO settings(id, data) VALUES (1, ?)
            ON CONFLICT(id) DO UPDATE SET data = excluded.data
            "#,
            params![data_str],
        )?;
        Ok(())
    }

    // ==========================================
    // Combos Operations
    // ==========================================

    /// Loads all model combos.
    pub fn load_combos(&self) -> anyhow::Result<Vec<Combo>> {
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare(
            "SELECT id, name, kind, models, strategy, matchRules, customPrompt, createdAt, updatedAt FROM combos ORDER BY createdAt ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let models_str: String = row.get(3)?;
            let models: Vec<String> = serde_json::from_str(&models_str).unwrap_or_default();
            Ok(Combo {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                models,
                strategy: row.get(4)?,
                match_rules: row.get(5)?,
                custom_prompt: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    /// Retrieves a combo by its unique name.
    pub fn get_combo_by_name(&self, name: &str) -> anyhow::Result<Option<Combo>> {
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare_cached(
            "SELECT id, name, kind, models, strategy, matchRules, customPrompt, createdAt, updatedAt FROM combos WHERE name = ?",
        )?;

        let result = stmt
            .query_row(params![name], |row| {
                let models_str: String = row.get(3)?;
                let models: Vec<String> = serde_json::from_str(&models_str).unwrap_or_default();
                Ok(Combo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    models,
                    strategy: row.get(4)?,
                    match_rules: row.get(5)?,
                    custom_prompt: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })
            .optional()?;

        Ok(result)
    }

    /// Retrieves a combo by its ID.
    pub fn get_combo_by_id(&self, id: &str) -> anyhow::Result<Option<Combo>> {
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare_cached(
            "SELECT id, name, kind, models, strategy, matchRules, customPrompt, createdAt, updatedAt FROM combos WHERE id = ?",
        )?;

        let result = stmt
            .query_row(params![id], |row| {
                let models_str: String = row.get(3)?;
                let models: Vec<String> = serde_json::from_str(&models_str).unwrap_or_default();
                Ok(Combo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    kind: row.get(2)?,
                    models,
                    strategy: row.get(4)?,
                    match_rules: row.get(5)?,
                    custom_prompt: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })
            .optional()?;

        Ok(result)
    }

    /// Saves or updates a combo.
    pub fn save_combo(&self, combo: &Combo) -> anyhow::Result<()> {
        let guard = self.conn.lock().unwrap();
        let models_json = serde_json::to_string(&combo.models)?;
        let strategy = combo.strategy.as_deref().unwrap_or("fallback");
        let match_rules = combo.match_rules.as_deref().unwrap_or("[]");
        let custom_prompt = combo.custom_prompt.as_deref().unwrap_or("");
        guard.execute(
            r#"
            INSERT INTO combos(id, name, kind, models, strategy, matchRules, customPrompt, createdAt, updatedAt)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, kind=excluded.kind, models=excluded.models,
                strategy=excluded.strategy, matchRules=excluded.matchRules,
                customPrompt=excluded.customPrompt, updatedAt=excluded.updatedAt
            "#,
            params![
                combo.id,
                combo.name,
                combo.kind,
                models_json,
                strategy,
                match_rules,
                custom_prompt,
                combo.created_at,
                combo.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Deletes a combo by ID or name.
    pub fn delete_combo(&self, id: &str) -> anyhow::Result<bool> {
        let guard = self.conn.lock().unwrap();
        let affected = guard.execute(
            "DELETE FROM combos WHERE id = ?1 OR name = ?1",
            params![id],
        )?;
        Ok(affected > 0)
    }

    /// Bulk deletes combos by IDs.
    pub fn bulk_delete_combos(&self, ids: &[String]) -> anyhow::Result<usize> {
        let guard = self.conn.lock().unwrap();
        let mut count = 0;
        for id in ids {
            let affected = guard.execute(
                "DELETE FROM combos WHERE id = ?1 OR name = ?1",
                params![id],
            )?;
            count += affected;
        }
        Ok(count)
    }

    /// Returns count of total rows in usageHistory.
    pub fn get_usage_count(&self) -> anyhow::Result<i64> {
        let guard = self.conn.lock().unwrap();
        let count: i64 =
            guard.query_row("SELECT COUNT(*) FROM usageHistory", [], |r| r.get(0))?;
        Ok(count)
    }

    /// Aggregates usage statistics for the given time period ("today", "24h", "7d", "30d", "60d", "all").
    pub fn get_usage_statistics(&self, period: &str) -> anyhow::Result<serde_json::Value> {
        let guard = self.conn.lock().unwrap();

        let cutoff = match period {
            "today" => {
                let now = chrono::Utc::now();
                now.date_naive()
                    .and_hms_opt(0, 0, 0)
                    .map(|dt| dt.and_utc().to_rfc3339())
                    .unwrap_or_default()
            }
            "24h" => (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339(),
            "7d" => (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339(),
            "30d" => (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339(),
            "60d" => (chrono::Utc::now() - chrono::Duration::days(60)).to_rfc3339(),
            _ => String::from("1970-01-01T00:00:00Z"),
        };

        // 1. Overall totals
        let (total_requests, prompt_tokens, completion_tokens, cached_tokens, total_cost): (
            i64,
            i64,
            i64,
            i64,
            f64,
        ) = guard
            .query_row(
                r#"
            SELECT
                COUNT(*),
                COALESCE(SUM(promptTokens), 0),
                COALESCE(SUM(completionTokens), 0),
                COALESCE(SUM(cachedTokens), 0),
                COALESCE(SUM(cost), 0.0)
            FROM usageHistory
            WHERE timestamp >= ?
            "#,
                [&cutoff],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap_or((0, 0, 0, 0, 0.0));

        // 2. By provider
        let mut by_provider = serde_json::Map::new();
        if let Ok(mut stmt) = guard.prepare(
            r#"
            SELECT
                provider,
                COUNT(*),
                COALESCE(SUM(promptTokens), 0),
                COALESCE(SUM(completionTokens), 0),
                COALESCE(SUM(cachedTokens), 0),
                COALESCE(SUM(cost), 0.0)
            FROM usageHistory
            WHERE timestamp >= ?
            GROUP BY provider
            "#,
        ) {
            if let Ok(rows) = stmt.query_map([&cutoff], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, f64>(5)?,
                ))
            }) {
                for item in rows.flatten() {
                    by_provider.insert(
                        item.0,
                        serde_json::json!({
                            "requests": item.1,
                            "promptTokens": item.2,
                            "completionTokens": item.3,
                            "cachedTokens": item.4,
                            "cost": item.5,
                        }),
                    );
                }
            }
        }

        // 3. By model
        let mut by_model = serde_json::Map::new();
        if let Ok(mut stmt) = guard.prepare(
            r#"
            SELECT
                model,
                provider,
                COUNT(*),
                COALESCE(SUM(promptTokens), 0),
                COALESCE(SUM(completionTokens), 0),
                COALESCE(SUM(cachedTokens), 0),
                COALESCE(SUM(cost), 0.0),
                MAX(timestamp)
            FROM usageHistory
            WHERE timestamp >= ?
            GROUP BY model, provider
            "#,
        ) {
            if let Ok(rows) = stmt.query_map([&cutoff], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, f64>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            }) {
                for item in rows.flatten() {
                    let key = format!("{} ({})", item.0, item.1);
                    by_model.insert(
                        key,
                        serde_json::json!({
                            "rawModel": item.0,
                            "provider": item.1,
                            "requests": item.2,
                            "promptTokens": item.3,
                            "completionTokens": item.4,
                            "cachedTokens": item.5,
                            "cost": item.6,
                            "lastUsed": item.7,
                        }),
                    );
                }
            }
        }

        // 4. By account / connection
        let mut by_account = serde_json::Map::new();
        if let Ok(mut stmt) = guard.prepare(
            r#"
            SELECT
                COALESCE(connectionId, 'default'),
                model,
                provider,
                COUNT(*),
                COALESCE(SUM(promptTokens), 0),
                COALESCE(SUM(completionTokens), 0),
                COALESCE(SUM(cachedTokens), 0),
                COALESCE(SUM(cost), 0.0),
                MAX(timestamp)
            FROM usageHistory
            WHERE timestamp >= ?
            GROUP BY connectionId, model, provider
            "#,
        ) {
            if let Ok(rows) = stmt.query_map([&cutoff], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, f64>(7)?,
                    r.get::<_, Option<String>>(8)?,
                ))
            }) {
                for item in rows.flatten() {
                    let key = format!("{} ({} - {})", item.1, item.2, item.0);
                    by_account.insert(
                        key,
                        serde_json::json!({
                            "connectionId": item.0,
                            "accountName": item.0,
                            "rawModel": item.1,
                            "provider": item.2,
                            "requests": item.3,
                            "promptTokens": item.4,
                            "completionTokens": item.5,
                            "cachedTokens": item.6,
                            "cost": item.7,
                            "lastUsed": item.8,
                        }),
                    );
                }
            }
        }

        // 5. Recent requests
        let mut recent_requests = Vec::new();
        if let Ok(mut stmt) = guard.prepare(
            r#"
            SELECT
                timestamp,
                provider,
                model,
                promptTokens,
                completionTokens,
                cachedTokens,
                status
            FROM usageHistory
            ORDER BY id DESC
            LIMIT 20
            "#,
        ) {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, String>(6)?,
                ))
            }) {
                for item in rows.flatten() {
                    recent_requests.push(serde_json::json!({
                        "timestamp": item.0,
                        "provider": item.1,
                        "model": item.2,
                        "promptTokens": item.3,
                        "completionTokens": item.4,
                        "cachedTokens": item.5,
                        "status": item.6,
                    }));
                }
            }
        }

        Ok(serde_json::json!({
            "totalRequests": total_requests,
            "totalPromptTokens": prompt_tokens,
            "totalCompletionTokens": completion_tokens,
            "totalCachedTokens": cached_tokens,
            "totalCost": total_cost,
            "byProvider": by_provider,
            "byModel": by_model,
            "byAccount": by_account,
            "byApiKey": serde_json::json!({}),
            "byEndpoint": serde_json::json!({}),
            "last10Minutes": [
                {"requests":0,"promptTokens":0,"completionTokens":0,"cost":0},
                {"requests":0,"promptTokens":0,"completionTokens":0,"cost":0},
                {"requests":0,"promptTokens":0,"completionTokens":0,"cost":0},
                {"requests":0,"promptTokens":0,"completionTokens":0,"cost":0},
                {"requests":0,"promptTokens":0,"completionTokens":0,"cost":0},
                {"requests":0,"promptTokens":0,"completionTokens":0,"cost":0},
                {"requests":0,"promptTokens":0,"completionTokens":0,"cost":0},
                {"requests":0,"promptTokens":0,"completionTokens":0,"cost":0},
                {"requests":0,"promptTokens":0,"completionTokens":0,"cost":0},
                {"requests":0,"promptTokens":0,"completionTokens":0,"cost":0}
            ],
            "pending": {
                "byModel": serde_json::json!({}),
                "byAccount": serde_json::json!({})
            },
            "activeRequests": [],
            "recentRequests": recent_requests,
            "errorProvider": ""
        }))
    }

    // ==========================================
    // Provider Nodes Operations
    // ==========================================

    /// Loads all provider nodes (custom endpoints with prefixes).
    pub fn load_provider_nodes(&self) -> anyhow::Result<Vec<ProviderNode>> {
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare(
            "SELECT id, type, name, data, createdAt, updatedAt FROM providerNodes ORDER BY createdAt ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let node_type: Option<String> = row.get(1)?;
            let name: Option<String> = row.get(2)?;
            let data_str: String = row.get(3)?;
            let created_at: String = row.get(4)?;
            let updated_at: String = row.get(5)?;

            let val: serde_json::Value = serde_json::from_str(&data_str).unwrap_or(serde_json::Value::Null);
            let prefix = val.get("prefix").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let base_url = val.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let api_type = val.get("apiType").and_then(|v| v.as_str()).map(|s| s.to_string());

            Ok(ProviderNode {
                id,
                node_type,
                name,
                prefix,
                base_url,
                api_type,
                data: val,
                created_at,
                updated_at,
            })
        })?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    /// Saves or updates a provider node.
    pub fn save_provider_node(&self, node: &ProviderNode) -> anyhow::Result<()> {
        let guard = self.conn.lock().unwrap();
        let data_str = serde_json::to_string(&node.data)?;
        guard.execute(
            r#"
            INSERT INTO providerNodes(id, type, name, data, createdAt, updatedAt)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                type=excluded.type, name=excluded.name, data=excluded.data, updatedAt=excluded.updatedAt
            "#,
            params![
                node.id,
                node.node_type,
                node.name,
                data_str,
                node.created_at,
                node.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Deletes a provider node by ID.
    pub fn delete_provider_node(&self, id: &str) -> anyhow::Result<bool> {
        let guard = self.conn.lock().unwrap();
        let affected = guard.execute(
            "DELETE FROM providerNodes WHERE id = ?1",
            params![id],
        )?;
        Ok(affected > 0)
    }

    /// Retrieves a single provider node by ID.
    pub fn get_provider_node_by_id(&self, id: &str) -> anyhow::Result<Option<ProviderNode>> {
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare_cached(
            "SELECT id, type, name, data, createdAt, updatedAt FROM providerNodes WHERE id = ?",
        )?;

        let result = stmt
            .query_row(params![id], |row| {
                let id: String = row.get(0)?;
                let node_type: Option<String> = row.get(1)?;
                let name: Option<String> = row.get(2)?;
                let data_str: String = row.get(3)?;
                let created_at: String = row.get(4)?;
                let updated_at: String = row.get(5)?;

                let val: serde_json::Value =
                    serde_json::from_str(&data_str).unwrap_or(serde_json::Value::Null);
                let prefix = val.get("prefix").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let base_url = val.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let api_type = val.get("apiType").and_then(|v| v.as_str()).map(|s| s.to_string());

                Ok(ProviderNode {
                    id,
                    node_type,
                    name,
                    prefix,
                    base_url,
                    api_type,
                    data: val,
                    created_at,
                    updated_at,
                })
            })
            .optional()?;

        Ok(result)
    }

    // ==========================================
    // Proxy Pools Operations
    // ==========================================

    /// Loads all proxy pool configurations.
    pub fn load_proxy_pools(&self) -> anyhow::Result<Vec<ProxyPool>> {
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare(
            "SELECT id, isActive, testStatus, data, createdAt, updatedAt FROM proxyPools ORDER BY createdAt ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let is_active: i64 = row.get(1)?;
            let test_status: Option<String> = row.get(2)?;
            let data_str: String = row.get(3)?;
            let created_at: String = row.get(4)?;
            let updated_at: String = row.get(5)?;
            let val: serde_json::Value = serde_json::from_str(&data_str).unwrap_or(serde_json::Value::Null);

            Ok(ProxyPool {
                id,
                is_active: is_active == 1,
                test_status,
                data: val,
                created_at,
                updated_at,
            })
        })?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    // ==========================================
    // Plans Operations
    // ==========================================

    /// Loads all rate-limit / token usage plans.
    pub fn load_plans(&self) -> anyhow::Result<Vec<Plan>> {
        let guard = self.conn.lock().unwrap();
        let mut stmt = guard.prepare(
            "SELECT id, tenant_id, name, description, limit_micros, limit_tokens, rpm_limit, tpm_limit, concurrency_limit, period, alert_pct, hard_cutoff, allowed_models, created_at, updated_at FROM plans ORDER BY created_at ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(Plan {
                id: row.get(0)?,
                tenant_id: row.get(1)?,
                name: row.get(2)?,
                description: row.get(3)?,
                limit_micros: row.get(4)?,
                limit_tokens: row.get(5)?,
                rpm_limit: row.get(6)?,
                tpm_limit: row.get(7)?,
                concurrency_limit: row.get(8)?,
                period: row.get(9)?,
                alert_pct: row.get(10)?,
                hard_cutoff: row.get::<_, i64>(11)? == 1,
                allowed_models: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
            })
        })?;

        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    // ==========================================
    // Recursive Combo Resolution
    // ==========================================

    /// Recursively expands a model or combo name into an ordered list of candidate models,
    /// terminating cycles safely with a visited set.
    pub fn resolve_combo_chain(&self, requested_model: &str) -> Vec<String> {
        let mut visited = std::collections::HashSet::new();
        let mut results = Vec::new();
        self.expand_combo_recursive(requested_model, &mut visited, &mut results);
        if results.is_empty() {
            results.push(requested_model.to_string());
        }
        results
    }

    fn expand_combo_recursive(
        &self,
        model: &str,
        visited: &mut std::collections::HashSet<String>,
        results: &mut Vec<String>,
    ) {
        if visited.contains(model) {
            return;
        }
        if let Ok(Some(combo)) = self.get_combo_by_name(model) {
            visited.insert(model.to_string());
            for sub in &combo.models {
                self.expand_combo_recursive(sub, visited, results);
            }
        } else {
            results.push(model.to_string());
        }
    }

    // ==========================================
    // Database Backup & Restore Operations
    // ==========================================

    /// Exports full database backup payload matching Next.js exportDb schema.
    pub fn export_database(&self) -> anyhow::Result<serde_json::Value> {
        let guard = self.conn.lock().unwrap();

        let settings = match guard.prepare_cached("SELECT data FROM settings WHERE id = 1") {
            Ok(mut stmt) => stmt
                .query_row([], |row| {
                    let data_str: String = row.get(0)?;
                    Ok(serde_json::from_str::<serde_json::Value>(&data_str)
                        .unwrap_or(serde_json::json!({})))
                })
                .optional()?
                .unwrap_or_else(|| serde_json::json!({})),
            Err(_) => serde_json::json!({}),
        };

        let mut provider_connections = Vec::new();
        if let Ok(mut stmt) = guard.prepare(
            "SELECT id, provider, authType, name, email, priority, isActive, data, createdAt, updatedAt FROM providerConnections",
        ) {
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let provider: String = row.get(1)?;
                let auth_type: String = row.get(2)?;
                let name: Option<String> = row.get(3)?;
                let email: Option<String> = row.get(4)?;
                let priority: Option<i64> = row.get(5)?;
                let is_active: i64 = row.get(6)?;
                let data_str: String = row.get(7)?;
                let created_at: String = row.get(8)?;
                let updated_at: String = row.get(9)?;

                let mut obj: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_str(&data_str).unwrap_or_default();
                obj.insert("id".to_string(), serde_json::json!(id));
                obj.insert("provider".to_string(), serde_json::json!(provider));
                obj.insert("authType".to_string(), serde_json::json!(auth_type));
                obj.insert("name".to_string(), serde_json::json!(name));
                obj.insert("email".to_string(), serde_json::json!(email));
                obj.insert("priority".to_string(), serde_json::json!(priority));
                obj.insert("isActive".to_string(), serde_json::json!(is_active == 1));
                obj.insert("createdAt".to_string(), serde_json::json!(created_at));
                obj.insert("updatedAt".to_string(), serde_json::json!(updated_at));
                Ok(serde_json::Value::Object(obj))
            })?;
            for r in rows {
                if let Ok(val) = r {
                    provider_connections.push(val);
                }
            }
        }

        let mut provider_nodes = Vec::new();
        if let Ok(mut stmt) = guard.prepare(
            "SELECT id, type, name, data, createdAt, updatedAt FROM providerNodes",
        ) {
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let node_type: Option<String> = row.get(1)?;
                let name: Option<String> = row.get(2)?;
                let data_str: String = row.get(3)?;
                let created_at: String = row.get(4)?;
                let updated_at: String = row.get(5)?;

                let mut obj: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_str(&data_str).unwrap_or_default();
                obj.insert("id".to_string(), serde_json::json!(id));
                obj.insert("type".to_string(), serde_json::json!(node_type));
                obj.insert("name".to_string(), serde_json::json!(name));
                obj.insert("createdAt".to_string(), serde_json::json!(created_at));
                obj.insert("updatedAt".to_string(), serde_json::json!(updated_at));
                Ok(serde_json::Value::Object(obj))
            })?;
            for r in rows {
                if let Ok(val) = r {
                    provider_nodes.push(val);
                }
            }
        }

        let mut proxy_pools = Vec::new();
        if let Ok(mut stmt) = guard.prepare(
            "SELECT id, isActive, testStatus, data, createdAt, updatedAt FROM proxyPools",
        ) {
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let is_active: i64 = row.get(1)?;
                let test_status: Option<String> = row.get(2)?;
                let data_str: String = row.get(3)?;
                let created_at: String = row.get(4)?;
                let updated_at: String = row.get(5)?;

                let mut obj: serde_json::Map<String, serde_json::Value> =
                    serde_json::from_str(&data_str).unwrap_or_default();
                obj.insert("id".to_string(), serde_json::json!(id));
                obj.insert("isActive".to_string(), serde_json::json!(is_active == 1));
                obj.insert("testStatus".to_string(), serde_json::json!(test_status));
                obj.insert("createdAt".to_string(), serde_json::json!(created_at));
                obj.insert("updatedAt".to_string(), serde_json::json!(updated_at));
                Ok(serde_json::Value::Object(obj))
            })?;
            for r in rows {
                if let Ok(val) = r {
                    proxy_pools.push(val);
                }
            }
        }

        let mut api_keys = Vec::new();
        if let Ok(mut stmt) = guard.prepare(
            "SELECT id, key, name, machineId, isActive, createdAt, rateLimitRequests, rateLimitWindowSeconds, tokenLimitDaily, tokenLimitMonthly, allowedModels, blockedModels, allowedIPs, blockedIPs, expiresAt, description, ownerRef, maxConcurrentRequests, allowedProviders, blockedProviders, forceProvider, forceModel, allowedEndpoints, blockedEndpoints, metadata, updatedAt FROM apiKeys",
        ) {
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let key: String = row.get(1)?;
                let name: Option<String> = row.get(2)?;
                let machine_id: Option<String> = row.get(3)?;
                let is_active: i64 = row.get(4)?;
                let created_at: String = row.get(5)?;
                let rate_limit_requests: i64 = row.get(6)?;
                let rate_limit_window_seconds: i64 = row.get(7)?;
                let token_limit_daily: i64 = row.get(8)?;
                let token_limit_monthly: i64 = row.get(9)?;
                let allowed_models: String = row.get(10)?;
                let blocked_models: String = row.get(11)?;
                let allowed_ips: String = row.get(12)?;
                let blocked_ips: String = row.get(13)?;
                let expires_at: String = row.get(14)?;
                let description: String = row.get(15)?;
                let owner_ref: Option<String> = row.get(16)?;
                let max_concurrent_requests: i64 = row.get(17)?;
                let allowed_providers: String = row.get(18)?;
                let blocked_providers: String = row.get(19)?;
                let force_provider: String = row.get(20)?;
                let force_model: String = row.get(21)?;
                let allowed_endpoints: String = row.get(22)?;
                let blocked_endpoints: String = row.get(23)?;
                let metadata: String = row.get(24)?;
                let updated_at: Option<String> = row.get(25)?;

                Ok(serde_json::json!({
                    "id": id,
                    "key": key,
                    "name": name,
                    "machineId": machine_id,
                    "isActive": is_active == 1,
                    "createdAt": created_at,
                    "rateLimitRequests": rate_limit_requests,
                    "rateLimitWindowSeconds": rate_limit_window_seconds,
                    "tokenLimitDaily": token_limit_daily,
                    "tokenLimitMonthly": token_limit_monthly,
                    "allowedModels": allowed_models,
                    "blockedModels": blocked_models,
                    "allowedIPs": allowed_ips,
                    "blockedIPs": blocked_ips,
                    "expiresAt": expires_at,
                    "description": description,
                    "ownerRef": owner_ref,
                    "maxConcurrentRequests": max_concurrent_requests,
                    "allowedProviders": allowed_providers,
                    "blockedProviders": blocked_providers,
                    "forceProvider": force_provider,
                    "forceModel": force_model,
                    "allowedEndpoints": allowed_endpoints,
                    "blockedEndpoints": blocked_endpoints,
                    "metadata": metadata,
                    "updatedAt": updated_at.unwrap_or(created_at),
                }))
            })?;
            for r in rows {
                if let Ok(val) = r {
                    api_keys.push(val);
                }
            }
        }

        let mut combos = Vec::new();
        if let Ok(mut stmt) = guard.prepare(
            "SELECT id, name, kind, models, strategy, matchRules, customPrompt, createdAt, updatedAt FROM combos",
        ) {
            let rows = stmt.query_map([], |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let kind: Option<String> = row.get(2)?;
                let models_str: String = row.get(3)?;
                let strategy: Option<String> = row.get(4)?;
                let match_rules: Option<String> = row.get(5)?;
                let custom_prompt: Option<String> = row.get(6)?;
                let created_at: String = row.get(7)?;
                let updated_at: String = row.get(8)?;

                let models: serde_json::Value =
                    serde_json::from_str(&models_str).unwrap_or(serde_json::json!([]));
                let match_rules_val: serde_json::Value = match_rules
                    .as_deref()
                    .map(|s| serde_json::from_str(s).unwrap_or(serde_json::json!([])))
                    .unwrap_or(serde_json::json!([]));

                Ok(serde_json::json!({
                    "id": id,
                    "name": name,
                    "kind": kind,
                    "models": models,
                    "strategy": strategy,
                    "matchRules": match_rules_val,
                    "customPrompt": custom_prompt,
                    "createdAt": created_at,
                    "updatedAt": updated_at,
                }))
            })?;
            for r in rows {
                if let Ok(val) = r {
                    combos.push(val);
                }
            }
        }

        let mut model_aliases = serde_json::Map::new();
        let mut custom_models = Vec::new();
        let mut mitm_alias = serde_json::Map::new();
        let mut pricing = serde_json::Map::new();

        if let Ok(mut stmt) = guard.prepare("SELECT scope, key, value FROM kv") {
            let rows = stmt.query_map([], |row| {
                let scope: String = row.get(0)?;
                let key: String = row.get(1)?;
                let value_str: String = row.get(2)?;
                let val: serde_json::Value =
                    serde_json::from_str(&value_str).unwrap_or(serde_json::Value::Null);
                Ok((scope, key, val))
            })?;
            for r in rows {
                if let Ok((scope, key, val)) = r {
                    match scope.as_str() {
                        "modelAliases" => {
                            model_aliases.insert(key, val);
                        }
                        "customModels" => {
                            custom_models.push(val);
                        }
                        "mitmAlias" => {
                            mitm_alias.insert(key, val);
                        }
                        "pricing" => {
                            pricing.insert(key, val);
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(serde_json::json!({
            "settings": settings,
            "providerConnections": provider_connections,
            "providerNodes": provider_nodes,
            "proxyPools": proxy_pools,
            "apiKeys": api_keys,
            "combos": combos,
            "modelAliases": model_aliases,
            "customModels": custom_models,
            "mitmAlias": mitm_alias,
            "pricing": pricing,
        }))
    }

    /// Replaces database contents with payload in a single atomic transaction.
    /// Returns per-table written row counts.
    pub fn import_database(&self, payload: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
        if !payload.is_object() {
            anyhow::bail!("Invalid database payload: root must be an object");
        }

        let has_known_table = payload.get("settings").is_some()
            || payload.get("providerConnections").is_some()
            || payload.get("providerNodes").is_some()
            || payload.get("proxyPools").is_some()
            || payload.get("apiKeys").is_some()
            || payload.get("combos").is_some()
            || payload.get("modelAliases").is_some()
            || payload.get("customModels").is_some()
            || payload.get("mitmAlias").is_some()
            || payload.get("pricing").is_some();

        if !has_known_table {
            anyhow::bail!("Invalid database payload: it contains none of the backup tables");
        }

        let mut guard = self.conn.lock().unwrap();
        let tx = guard.transaction()?;

        // Wipe tables (preserve _meta and usageHistory)
        tx.execute_batch(
            r#"
            DELETE FROM settings;
            DELETE FROM providerConnections;
            DELETE FROM providerNodes;
            DELETE FROM proxyPools;
            DELETE FROM apiKeys;
            DELETE FROM combos;
            DELETE FROM kv WHERE scope IN ('modelAliases', 'customModels', 'mitmAlias', 'pricing');
            "#,
        )?;

        let mut counts = serde_json::Map::new();

        // 1. Settings
        if let Some(settings_val) = payload.get("settings") {
            if settings_val.is_object() {
                let data_str = serde_json::to_string(settings_val)?;
                tx.execute(
                    "INSERT INTO settings(id, data) VALUES(1, ?) ON CONFLICT(id) DO UPDATE SET data = excluded.data",
                    params![data_str],
                )?;
                counts.insert("settings".to_string(), serde_json::json!(1));
            } else {
                counts.insert("settings".to_string(), serde_json::json!(0));
            }
        } else {
            counts.insert("settings".to_string(), serde_json::json!(0));
        }

        // 2. providerConnections
        let mut pc_count = 0;
        if let Some(pcs) = payload.get("providerConnections").and_then(|v| v.as_array()) {
            for c in pcs {
                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                let provider = c.get("provider").and_then(|v| v.as_str()).unwrap_or_default();
                let auth_type = c.get("authType").and_then(|v| v.as_str()).unwrap_or("oauth");
                let name = c.get("name").and_then(|v| v.as_str());
                let email = c.get("email").and_then(|v| v.as_str());
                let priority = c.get("priority").and_then(|v| v.as_i64());
                let is_active = if c.get("isActive").and_then(|v| v.as_bool()) == Some(false) { 0 } else { 1 };
                let now = Utc::now().to_rfc3339();
                let created_at = c.get("createdAt").and_then(|v| v.as_str()).unwrap_or(&now);
                let updated_at = c.get("updatedAt").and_then(|v| v.as_str()).unwrap_or(&now);
                let data_str = serde_json::to_string(c)?;

                tx.execute(
                    r#"
                    INSERT OR REPLACE INTO providerConnections(id, provider, authType, name, email, priority, isActive, data, createdAt, updatedAt)
                    VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                    params![id, provider, auth_type, name, email, priority, is_active, data_str, created_at, updated_at],
                )?;
                pc_count += 1;
            }
        }
        counts.insert("providerConnections".to_string(), serde_json::json!(pc_count));

        // 3. providerNodes
        let mut pn_count = 0;
        if let Some(pns) = payload.get("providerNodes").and_then(|v| v.as_array()) {
            for n in pns {
                let id = n.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                let node_type = n.get("type").and_then(|v| v.as_str());
                let name = n.get("name").and_then(|v| v.as_str());
                let now = Utc::now().to_rfc3339();
                let created_at = n.get("createdAt").and_then(|v| v.as_str()).unwrap_or(&now);
                let updated_at = n.get("updatedAt").and_then(|v| v.as_str()).unwrap_or(&now);
                let data_str = serde_json::to_string(n)?;

                tx.execute(
                    r#"
                    INSERT OR REPLACE INTO providerNodes(id, type, name, data, createdAt, updatedAt)
                    VALUES(?, ?, ?, ?, ?, ?)
                    "#,
                    params![id, node_type, name, data_str, created_at, updated_at],
                )?;
                pn_count += 1;
            }
        }
        counts.insert("providerNodes".to_string(), serde_json::json!(pn_count));

        // 4. proxyPools
        let mut pp_count = 0;
        if let Some(pps) = payload.get("proxyPools").and_then(|v| v.as_array()) {
            for p in pps {
                let id = p.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                let is_active = if p.get("isActive").and_then(|v| v.as_bool()) == Some(false) { 0 } else { 1 };
                let test_status = p.get("testStatus").and_then(|v| v.as_str()).unwrap_or("unknown");
                let now = Utc::now().to_rfc3339();
                let created_at = p.get("createdAt").and_then(|v| v.as_str()).unwrap_or(&now);
                let updated_at = p.get("updatedAt").and_then(|v| v.as_str()).unwrap_or(&now);
                let data_str = serde_json::to_string(p)?;

                tx.execute(
                    r#"
                    INSERT OR REPLACE INTO proxyPools(id, isActive, testStatus, data, createdAt, updatedAt)
                    VALUES(?, ?, ?, ?, ?, ?)
                    "#,
                    params![id, is_active, test_status, data_str, created_at, updated_at],
                )?;
                pp_count += 1;
            }
        }
        counts.insert("proxyPools".to_string(), serde_json::json!(pp_count));

        // 5. apiKeys
        let mut ak_count = 0;
        if let Some(aks) = payload.get("apiKeys").and_then(|v| v.as_array()) {
            for k in aks {
                let id = k.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                let key = k.get("key").and_then(|v| v.as_str()).unwrap_or_default();
                if id.is_empty() || key.is_empty() {
                    continue;
                }
                let name = k.get("name").and_then(|v| v.as_str());
                let machine_id = k.get("machineId").and_then(|v| v.as_str());
                let is_active = if k.get("isActive").and_then(|v| v.as_bool()) == Some(false) { 0 } else { 1 };
                let now = Utc::now().to_rfc3339();
                let created_at = k.get("createdAt").and_then(|v| v.as_str()).unwrap_or(&now);
                let rate_limit_requests = k.get("rateLimitRequests").and_then(|v| v.as_i64()).unwrap_or(0);
                let rate_limit_window_seconds = k.get("rateLimitWindowSeconds").and_then(|v| v.as_i64()).unwrap_or(60);
                let token_limit_daily = k.get("tokenLimitDaily").and_then(|v| v.as_i64()).unwrap_or(0);
                let token_limit_monthly = k.get("tokenLimitMonthly").and_then(|v| v.as_i64()).unwrap_or(0);
                let allowed_models = k.get("allowedModels").and_then(|v| v.as_str()).unwrap_or("");
                let blocked_models = k.get("blockedModels").and_then(|v| v.as_str()).unwrap_or("");
                let allowed_ips = k.get("allowedIPs").and_then(|v| v.as_str()).unwrap_or("");
                let blocked_ips = k.get("blockedIPs").and_then(|v| v.as_str()).unwrap_or("");
                let expires_at = k.get("expiresAt").and_then(|v| v.as_str()).unwrap_or("");
                let description = k.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let owner_ref = k.get("ownerRef").and_then(|v| v.as_str());
                let max_concurrent_requests = k.get("maxConcurrentRequests").and_then(|v| v.as_i64()).unwrap_or(0);
                let allowed_providers = k.get("allowedProviders").and_then(|v| v.as_str()).unwrap_or("");
                let blocked_providers = k.get("blockedProviders").and_then(|v| v.as_str()).unwrap_or("");
                let force_provider = k.get("forceProvider").and_then(|v| v.as_str()).unwrap_or("");
                let force_model = k.get("forceModel").and_then(|v| v.as_str()).unwrap_or("");
                let allowed_endpoints = k.get("allowedEndpoints").and_then(|v| v.as_str()).unwrap_or("");
                let blocked_endpoints = k.get("blockedEndpoints").and_then(|v| v.as_str()).unwrap_or("");
                let metadata = k.get("metadata").map(|v| if v.is_string() { v.as_str().unwrap().to_string() } else { serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()) }).unwrap_or_else(|| "{}".to_string());
                let updated_at = k.get("updatedAt").and_then(|v| v.as_str()).unwrap_or(created_at);

                tx.execute(
                    r#"
                    INSERT OR REPLACE INTO apiKeys(id, key, name, machineId, isActive, createdAt, rateLimitRequests, rateLimitWindowSeconds, tokenLimitDaily, tokenLimitMonthly, allowedModels, blockedModels, allowedIPs, blockedIPs, expiresAt, description, ownerRef, maxConcurrentRequests, allowedProviders, blockedProviders, forceProvider, forceModel, allowedEndpoints, blockedEndpoints, metadata, updatedAt)
                    VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                    params![id, key, name, machine_id, is_active, created_at, rate_limit_requests, rate_limit_window_seconds, token_limit_daily, token_limit_monthly, allowed_models, blocked_models, allowed_ips, blocked_ips, expires_at, description, owner_ref, max_concurrent_requests, allowed_providers, blocked_providers, force_provider, force_model, allowed_endpoints, blocked_endpoints, metadata, updated_at],
                )?;
                ak_count += 1;
            }
        }
        counts.insert("apiKeys".to_string(), serde_json::json!(ak_count));

        // 6. combos
        let mut combo_count = 0;
        if let Some(cbs) = payload.get("combos").and_then(|v| v.as_array()) {
            for c in cbs {
                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                let name = c.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                if id.is_empty() || name.is_empty() {
                    continue;
                }
                let kind = c.get("kind").and_then(|v| v.as_str());
                let models_str = serde_json::to_string(c.get("models").unwrap_or(&serde_json::json!([])))?;
                let strategy = c.get("strategy").and_then(|v| v.as_str()).unwrap_or("fallback");
                let match_rules = serde_json::to_string(c.get("matchRules").unwrap_or(&serde_json::json!([])))?;
                let custom_prompt = c.get("customPrompt").and_then(|v| v.as_str()).unwrap_or("");
                let now = Utc::now().to_rfc3339();
                let created_at = c.get("createdAt").and_then(|v| v.as_str()).unwrap_or(&now);
                let updated_at = c.get("updatedAt").and_then(|v| v.as_str()).unwrap_or(&now);

                tx.execute(
                    r#"
                    INSERT OR REPLACE INTO combos(id, name, kind, models, strategy, matchRules, customPrompt, createdAt, updatedAt)
                    VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                    params![id, name, kind, models_str, strategy, match_rules, custom_prompt, created_at, updated_at],
                )?;
                combo_count += 1;
            }
        }
        counts.insert("combos".to_string(), serde_json::json!(combo_count));

        // 7. modelAliases
        let mut alias_count = 0;
        if let Some(map) = payload.get("modelAliases").and_then(|v| v.as_object()) {
            for (k, v) in map {
                let val_str = serde_json::to_string(v)?;
                tx.execute(
                    "INSERT OR REPLACE INTO kv(scope, key, value) VALUES('modelAliases', ?, ?)",
                    params![k, val_str],
                )?;
                alias_count += 1;
            }
        }
        counts.insert("modelAliases".to_string(), serde_json::json!(alias_count));

        // 8. customModels
        let mut cm_count = 0;
        if let Some(cms) = payload.get("customModels").and_then(|v| v.as_array()) {
            for m in cms {
                let provider_alias = m.get("providerAlias").and_then(|v| v.as_str()).unwrap_or("custom");
                let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("default");
                let model_type = m.get("type").and_then(|v| v.as_str()).unwrap_or("llm");
                let key = format!("{}|{}|{}", provider_alias, id, model_type);
                let val_str = serde_json::to_string(m)?;
                tx.execute(
                    "INSERT OR REPLACE INTO kv(scope, key, value) VALUES('customModels', ?, ?)",
                    params![key, val_str],
                )?;
                cm_count += 1;
            }
        }
        counts.insert("customModels".to_string(), serde_json::json!(cm_count));

        // 9. mitmAlias
        let mut mitm_count = 0;
        if let Some(map) = payload.get("mitmAlias").and_then(|v| v.as_object()) {
            for (k, v) in map {
                let val_str = serde_json::to_string(v)?;
                tx.execute(
                    "INSERT OR REPLACE INTO kv(scope, key, value) VALUES('mitmAlias', ?, ?)",
                    params![k, val_str],
                )?;
                mitm_count += 1;
            }
        }
        counts.insert("mitmAlias".to_string(), serde_json::json!(mitm_count));

        // 10. pricing
        let mut pricing_count = 0;
        if let Some(map) = payload.get("pricing").and_then(|v| v.as_object()) {
            for (k, v) in map {
                let val_str = serde_json::to_string(v)?;
                tx.execute(
                    "INSERT OR REPLACE INTO kv(scope, key, value) VALUES('pricing', ?, ?)",
                    params![k, val_str],
                )?;
                pricing_count += 1;
            }
        }
        counts.insert("pricing".to_string(), serde_json::json!(pricing_count));

        tx.commit()?;

        Ok(serde_json::Value::Object(counts))
    }
}

/// Helper to match model patterns like `*`, `openai/*`, or `gpt-4o`.
fn pattern_matches(pattern: &str, target: &str) -> bool {
    let p = pattern.trim();
    let t = target.trim();

    if p == "*" || p == t {
        return true;
    }

    if let Some(prefix) = p.strip_suffix("/*") {
        if let Some((target_prefix, _)) = t.split_once('/') {
            return prefix.eq_ignore_ascii_case(target_prefix);
        }
    }

    // If pattern doesn't include provider prefix but target does (e.g. pattern="gpt-4o", target="openai/gpt-4o")
    if let Some((_, model_part)) = t.split_once('/') {
        if p.eq_ignore_ascii_case(model_part) {
            return true;
        }
    }

    // If pattern includes provider prefix but target doesn't (e.g. pattern="openai/gpt-4o", target="gpt-4o")
    if let Some((_, pattern_model)) = p.split_once('/') {
        if pattern_model.eq_ignore_ascii_case(t) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_database_init_and_key_validation() {
        let db = Database::open_in_memory().expect("open in memory db");

        let key = ApiKey {
            id: "key-1".to_string(),
            key: "sk-test-secret-123".to_string(),
            name: Some("Test Key".to_string()),
            machine_id: None,
            is_active: true,
            created_at: Utc::now().to_rfc3339(),
            rate_limit_requests: 2,
            rate_limit_window_seconds: 60,
            token_limit_daily: 0,
            token_limit_monthly: 0,
            allowed_models: "openai/*,claude-3-5-sonnet".to_string(),
            blocked_models: "openai/gpt-3.5-turbo".to_string(),
            allowed_ips: "".to_string(),
            blocked_ips: "".to_string(),
            expires_at: "".to_string(),
            description: "".to_string(),
            owner_ref: None,
            max_concurrent_requests: 0,
            allowed_providers: "".to_string(),
            blocked_providers: "".to_string(),
            force_provider: "".to_string(),
            force_model: "".to_string(),
            allowed_endpoints: "".to_string(),
            blocked_endpoints: "".to_string(),
        };

        db.save_api_key(&key).expect("save api key");

        // Verify key lookup
        let found = db
            .get_api_key_by_key("sk-test-secret-123")
            .expect("lookup key")
            .expect("key must be found");
        assert_eq!(found.id, "key-1");
        assert_eq!(found.name.as_deref(), Some("Test Key"));

        // Validation passes
        assert!(db.validate_key(&found).is_ok());

        // Model checks
        assert!(db.check_model_allowed(&found, "openai/gpt-4o").is_ok());
        assert!(db
            .check_model_allowed(&found, "anthropic/claude-3-5-sonnet")
            .is_ok());
        // Blocked model
        assert_eq!(
            db.check_model_allowed(&found, "openai/gpt-3.5-turbo"),
            Err(KeyValidationError::ModelBlocked(
                "openai/gpt-3.5-turbo".to_string()
            ))
        );
        // Not in allowlist
        assert_eq!(
            db.check_model_allowed(&found, "google/gemini-pro"),
            Err(KeyValidationError::ModelNotAllowed(
                "google/gemini-pro".to_string()
            ))
        );

        // Rate limiting test: limit is 2
        assert!(db.validate_key(&found).is_ok()); // request 2
        let rate_limited = db.validate_key(&found); // request 3 -> limit exceeded
        assert!(matches!(
            rate_limited,
            Err(KeyValidationError::RateLimited { .. })
        ));
    }

    #[tokio::test]
    async fn test_provider_connections_loader_and_parser() {
        let db = Database::open_in_memory().expect("open in memory db");

        let raw_json = r#"{"apiKey": "sk-proj-xyz", "baseUrl": "https://api.openai.com/v1", "headers": {"X-Custom": "Val"}}"#;
        let parsed_data: ConnectionData = serde_json::from_str(raw_json).expect("parse json");

        let conn = ProviderConnection {
            id: "conn-openai-1".to_string(),
            provider: "openai".to_string(),
            auth_type: "api_key".to_string(),
            name: Some("Main OpenAI".to_string()),
            email: None,
            priority: Some(1),
            is_active: true,
            data: parsed_data,
            raw_data: raw_json.to_string(),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };

        db.save_provider_connection(&conn)
            .expect("save connection");

        let loaded = db
            .load_provider_connections()
            .expect("load provider connections");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].provider, "openai");
        assert_eq!(loaded[0].data.api_key.as_deref(), Some("sk-proj-xyz"));
        assert_eq!(
            loaded[0].data.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            loaded[0]
                .data
                .headers
                .as_ref()
                .and_then(|h| h.get("X-Custom"))
                .map(|s| s.as_str()),
            Some("Val")
        );
    }

    #[tokio::test]
    async fn test_write_behind_usage_queue() {
        let db = Database::open_in_memory().expect("open in memory db");

        for i in 0..10 {
            db.queue_usage(UsageRecord {
                timestamp: Utc::now().to_rfc3339(),
                provider: Some("openai".to_string()),
                model: Some("gpt-4o".to_string()),
                prompt_tokens: 10 + i,
                completion_tokens: 20 + i,
                cost: 0.005,
                status: "ok".to_string(),
                ..Default::default()
            });
        }

        // Wait for background worker interval to flush
        tokio::time::sleep(Duration::from_millis(700)).await;

        let count = db.get_usage_count().expect("get usage count");
        assert_eq!(count, 10);
    }

    #[tokio::test]
    async fn test_settings_and_combos() {
        let db = Database::open_in_memory().expect("open in memory db");

        let settings = serde_json::json!({
            "requireApiKey": false,
            "customSetting": "active"
        });
        db.save_settings(&settings).expect("save settings");
        let loaded_settings = db.load_settings().expect("load settings");
        assert_eq!(loaded_settings["requireApiKey"], false);
        assert_eq!(loaded_settings["customSetting"], "active");

        let combo = Combo {
            id: "combo-1".to_string(),
            name: "smart-fallback".to_string(),
            kind: Some("fallback".to_string()),
            models: vec![
                "anthropic/claude-3-5-sonnet".to_string(),
                "openai/gpt-4o".to_string(),
            ],
            strategy: Some("fallback".to_string()),
            match_rules: Some("[]".to_string()),
            custom_prompt: Some("".to_string()),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        db.save_combo(&combo).expect("save combo");

        let loaded_combo = db
            .get_combo_by_name("smart-fallback")
            .expect("get combo")
            .expect("combo must exist");
        assert_eq!(loaded_combo.models.len(), 2);
        assert_eq!(loaded_combo.models[0], "anthropic/claude-3-5-sonnet");

        // Test combo recursive expansion & cycle protection
        let combo_a = Combo {
            id: "combo-a".to_string(),
            name: "group-a".to_string(),
            kind: None,
            models: vec!["group-b".to_string(), "leaf-1".to_string()],
            strategy: None,
            match_rules: None,
            custom_prompt: None,
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        let combo_b = Combo {
            id: "combo-b".to_string(),
            name: "group-b".to_string(),
            kind: None,
            models: vec!["group-a".to_string(), "leaf-2".to_string()],
            strategy: None,
            match_rules: None,
            custom_prompt: None,
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        db.save_combo(&combo_a).expect("save a");
        db.save_combo(&combo_b).expect("save b");

        let chain = db.resolve_combo_chain("group-a");
        assert_eq!(chain, vec!["leaf-2".to_string(), "leaf-1".to_string()]);

        let plain_chain = db.resolve_combo_chain("non-existent-model");
        assert_eq!(plain_chain, vec!["non-existent-model".to_string()]);
    }

    #[tokio::test]
    async fn test_export_and_import_database() {
        let db = Database::open_in_memory().expect("open db");

        // 1. Seed some data
        let settings = serde_json::json!({
            "requireLogin": true,
            "outboundProxyEnabled": false,
            "customTag": "test-v1"
        });
        db.save_settings(&settings).expect("save settings");

        let combo = Combo {
            id: "combo-1".to_string(),
            name: "smart-fallback".to_string(),
            kind: Some("text".to_string()),
            models: vec!["openai/gpt-4o".to_string(), "anthropic/claude-3-5-sonnet".to_string()],
            strategy: Some("fallback".to_string()),
            match_rules: Some("[]".to_string()),
            custom_prompt: Some("be helpful".to_string()),
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };
        db.save_combo(&combo).expect("save combo");

        // 2. Export database
        let exported = db.export_database().expect("export database");
        assert_eq!(exported["settings"]["customTag"], "test-v1");
        assert_eq!(exported["combos"].as_array().unwrap().len(), 1);
        assert_eq!(exported["combos"][0]["name"], "smart-fallback");

        // 3. Create fresh db and import exported data
        let db2 = Database::open_in_memory().expect("open db2");
        let counts = db2.import_database(&exported).expect("import database");
        assert_eq!(counts["settings"], 1);
        assert_eq!(counts["combos"], 1);

        let reloaded_settings = db2.load_settings().expect("load settings db2");
        assert_eq!(reloaded_settings["customTag"], "test-v1");

        let reloaded_combos = db2.load_combos().expect("load combos db2");
        assert_eq!(reloaded_combos.len(), 1);
        assert_eq!(reloaded_combos[0].name, "smart-fallback");
    }
}
