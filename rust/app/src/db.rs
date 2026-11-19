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
                createdAt TEXT NOT NULL,
                updatedAt TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_combo_name ON combos(name);

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
            "SELECT id, name, kind, models, createdAt, updatedAt FROM combos ORDER BY createdAt ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let models_str: String = row.get(3)?;
            let models: Vec<String> = serde_json::from_str(&models_str).unwrap_or_default();
            Ok(Combo {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                models,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
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
            "SELECT id, name, kind, models, createdAt, updatedAt FROM combos WHERE name = ?",
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
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })
            .optional()?;

        Ok(result)
    }

    /// Saves or updates a combo.
    pub fn save_combo(&self, combo: &Combo) -> anyhow::Result<()> {
        let guard = self.conn.lock().unwrap();
        let models_json = serde_json::to_string(&combo.models)?;
        guard.execute(
            r#"
            INSERT INTO combos(id, name, kind, models, createdAt, updatedAt)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, kind=excluded.kind, models=excluded.models, updatedAt=excluded.updatedAt
            "#,
            params![
                combo.id,
                combo.name,
                combo.kind,
                models_json,
                combo.created_at,
                combo.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Returns count of total rows in usageHistory.
    pub fn get_usage_count(&self) -> anyhow::Result<i64> {
        let guard = self.conn.lock().unwrap();
        let count: i64 =
            guard.query_row("SELECT COUNT(*) FROM usageHistory", [], |r| r.get(0))?;
        Ok(count)
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
    }
}
