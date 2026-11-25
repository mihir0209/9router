use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use ninerouter::config::Config;
use ninerouter::db::{
    ApiKey, Combo, ConnectionData, Database, ProviderConnection, ProviderNode, ProxyPool,
    RequestDetail, UsageRecord,
};
use serde_json::json;

fn live_pg_url() -> Option<String> {
    std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("PG_TEST_URL"))
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| {
            let lower = s.to_ascii_lowercase();
            lower.starts_with("postgres://") || lower.starts_with("postgresql://")
        })
}

fn pg_config_for(url: &str, data_dir: &str) -> Config {
    let mut cfg = Config::default();
    cfg.data_dir = PathBuf::from(data_dir);
    cfg.db_path = cfg.data_dir.join("db").join("data.sqlite");
    cfg.storage.data_dir = cfg.data_dir.clone();
    cfg.storage.db_path = cfg.db_path.clone();
    cfg.storage.database_url = Some(url.to_string());
    cfg.storage.db_pool_max = 8;
    cfg.storage.db_ssl = false;
    cfg
}

fn make_api_key(id: &str, key: &str, owner: Option<&str>) -> ApiKey {
    ApiKey {
        id: id.to_string(),
        key: key.to_string(),
        name: Some(format!("Key {}", id)),
        is_active: true,
        created_at: Utc::now().to_rfc3339(),
        rate_limit_requests: 0,
        rate_limit_window_seconds: 60,
        allowed_models: "*".to_string(),
        owner_ref: owner.map(|s| s.to_string()),
        budget_usd: Some(125.50),
        budget_reset_hours: Some(24.5),
        ..Default::default()
    }
}

fn make_provider_conn(id: &str, provider: &str, priority: i64, active: bool) -> ProviderConnection {
    let raw = r#"{"apiKey":"sk-provider-secret","baseUrl":"https://api.openai.com/v1"}"#.to_string();
    let data: ConnectionData = serde_json::from_str(&raw).unwrap_or_default();
    ProviderConnection {
        id: id.to_string(),
        provider: provider.to_string(),
        auth_type: "api_key".to_string(),
        name: Some(format!("Conn {}", id)),
        email: Some("ops@example.com".to_string()),
        priority: Some(priority),
        is_active: active,
        data,
        raw_data: raw,
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
    }
}

#[tokio::test]
async fn test_pg_bootstrap_idempotency_and_ddl_isolation() {
    let Some(url) = live_pg_url() else {
        eprintln!("[SKIP] TEST_DATABASE_URL / DATABASE_URL unset; skipping live Postgres isolation test");
        return;
    };

    let nonce = Utc::now().timestamp_micros();
    let dir_a = format!("/tmp/nr_pg_iso_a_{}_{}", std::process::id(), nonce);
    let dir_b = format!("/tmp/nr_pg_iso_b_{}_{}", std::process::id(), nonce);

    let cfg_a = pg_config_for(&url, &dir_a);
    let cfg_b = pg_config_for(&url, &dir_b);

    let db_a = Database::open(&cfg_a).expect("open Postgres instance A");
    let db_b = Database::open(&cfg_b).expect("open Postgres instance B");

    assert!(db_a.is_postgres());
    assert!(db_b.is_postgres());
    assert!(db_a.is_pooled());
    let schema_a = db_a.pg_schema().expect("schema A");
    let schema_b = db_b.pg_schema().expect("schema B");
    assert!(schema_a.starts_with("nr_"));
    assert!(schema_b.starts_with("nr_"));
    assert_ne!(schema_a, schema_b, "Different DATA_DIRs must isolate schemas");

    // Re-opening on same DATA_DIR must run DDL + migrations idempotently
    let db_a2 = Database::open(&cfg_a).expect("re-open Postgres instance A idempotently");
    assert_eq!(db_a2.pg_schema().as_deref(), Some(schema_a.as_str()));

    // Write in A -> must NOT leak into B
    let key_a = make_api_key("iso-key-a", &format!("sk-iso-a-{}", nonce), Some("tenant-a"));
    db_a.save_api_key(&key_a).expect("save key in schema A");
    db_a.save_settings(&json!({ "requireApiKey": false, "tenantTag": "alpha" }))
        .expect("save settings in schema A");

    assert!(db_a2.get_api_key_by_key(&key_a.key).unwrap().is_some());
    assert!(
        db_b.get_api_key_by_key(&key_a.key).unwrap().is_none(),
        "Schema B must not see Schema A apiKey"
    );
    let settings_b = db_b.load_settings().expect("load default settings B");
    assert!(settings_b.get("tenantTag").is_none());

    // Cleanup test schemas
    if let Some(pg) = db_a.pg_backend() {
        let _ = pg.export_database();
    }
    drop(db_a2);
    drop(db_a);
    drop(db_b);
}

#[tokio::test]
async fn test_pg_multi_instance_shared_state_and_full_crud() {
    let Some(url) = live_pg_url() else {
        eprintln!("[SKIP] TEST_DATABASE_URL / DATABASE_URL unset; skipping live Postgres CRUD test");
        return;
    };

    let nonce = Utc::now().timestamp_micros();
    let shared_dir = format!("/tmp/nr_pg_shared_{}_{}", std::process::id(), nonce);
    let cfg = pg_config_for(&url, &shared_dir);

    let db1 = Database::open(&cfg).expect("open shared Postgres instance 1");
    let db2 = Database::open(&cfg).expect("open shared Postgres instance 2");
    assert_eq!(db1.pg_schema(), db2.pg_schema());

    // 1. API Keys CRUD + immediate cross-instance lookup + regenerate + delete
    let raw_key = format!("sk-live-pg-{}", nonce);
    let ak = make_api_key("pg-key-1", &raw_key, Some("owner-42"));
    db1.save_api_key(&ak).expect("save api key");

    let found_on_db2 = db2
        .get_api_key_by_key(&raw_key)
        .expect("lookup on db2")
        .expect("key visible immediately on db2");
    assert_eq!(found_on_db2.id, "pg-key-1");
    assert_eq!(found_on_db2.budget_usd, Some(125.50));
    assert_eq!(found_on_db2.budget_reset_hours, Some(24.5));
    assert!(db2.validate_key(&found_on_db2).is_ok());

    let by_owner = db2.list_api_keys_by_owner("owner-42").expect("list by owner");
    assert_eq!(by_owner.len(), 1);

    let regen = db1
        .regenerate_api_key("pg-key-1")
        .expect("regen")
        .expect("regen key exists");
    assert_ne!(regen.key, raw_key);
    assert_eq!(regen.regenerate_version, 1);

    // 2. Provider Connections
    let pc1 = make_provider_conn("pc-1", "openai", 1, true);
    let pc2 = make_provider_conn("pc-2", "anthropic", 2, false);
    db1.save_provider_connection(&pc1).expect("save pc1");
    db1.save_provider_connection(&pc2).expect("save pc2");
    let active_openai = db1
        .get_active_provider_connections(Some("openai"))
        .expect("active openai conns");
    assert_eq!(active_openai.len(), 1);
    assert_eq!(active_openai[0].id, "pc-1");

    // 3. Provider Nodes
    let node = ProviderNode {
        id: "pn-1".to_string(),
        node_type: Some("openai-compatible".to_string()),
        name: Some("Custom vLLM".to_string()),
        prefix: "vllm".to_string(),
        base_url: "http://127.0.0.1:8000/v1".to_string(),
        api_type: Some("chat".to_string()),
        data: json!({
            "prefix": "vllm",
            "baseUrl": "http://127.0.0.1:8000/v1",
            "apiType": "chat"
        }),
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
    };
    db1.save_provider_node(&node).expect("save node");
    assert_eq!(
        db1.get_provider_node_by_id("pn-1")
            .unwrap()
            .unwrap()
            .base_url,
        "http://127.0.0.1:8000/v1"
    );

    // 4. Proxy Pools
    let pool1 = ProxyPool {
        id: "pp-active".to_string(),
        is_active: true,
        test_status: Some("ok".to_string()),
        data: json!({ "name": "US Proxy", "proxyUrl": "http://127.0.0.1:8888", "source": "manual" }),
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
    };
    let pool2 = ProxyPool {
        id: "pp-dead".to_string(),
        is_active: false,
        test_status: Some("failed".to_string()),
        data: json!({ "name": "Dead Proxy", "proxyUrl": "http://127.0.0.1:9999", "source": "proxyscrape" }),
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
    };
    db1.save_proxy_pool(&pool1).expect("save pool1");
    db1.save_proxy_pool(&pool2).expect("save pool2");
    let (deleted_cnt, _, _) = db1
        .delete_inactive_proxy_pools(&[])
        .expect("delete inactive pools");
    assert_eq!(deleted_cnt, 1);
    assert!(db1.get_proxy_pool_by_id("pp-active").unwrap().is_some());
    assert!(db1.get_proxy_pool_by_id("pp-dead").unwrap().is_none());

    // 5. Combos + recursive chain
    let combo_leaf = Combo {
        id: "c-leaf".to_string(),
        name: "tier-2".to_string(),
        kind: Some("fallback".to_string()),
        models: vec!["anthropic/claude-3-5-sonnet".to_string()],
        strategy: Some("fallback".to_string()),
        match_rules: Some("[]".to_string()),
        custom_prompt: Some("".to_string()),
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
    };
    let combo_top = Combo {
        id: "c-top".to_string(),
        name: "tier-1".to_string(),
        kind: Some("fallback".to_string()),
        models: vec!["openai/gpt-4o".to_string(), "tier-2".to_string()],
        strategy: Some("fallback".to_string()),
        match_rules: Some("[]".to_string()),
        custom_prompt: Some("".to_string()),
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
    };
    db1.save_combo(&combo_leaf).expect("save combo_leaf");
    db1.save_combo(&combo_top).expect("save combo_top");
    let chain = db1.resolve_combo_chain("tier-1");
    assert_eq!(
        chain,
        vec![
            "openai/gpt-4o".to_string(),
            "anthropic/claude-3-5-sonnet".to_string()
        ]
    );

    // 6. Settings + KV + LISTEN/NOTIFY propagation
    db1.save_settings(&json!({ "requireApiKey": false, "sharedFlag": "yes" }))
        .expect("save settings");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let s2 = db2.load_settings().expect("load settings on db2 via NOTIFY");
    assert_eq!(s2["sharedFlag"], "yes");

    db1.set_model_alias("fast", "openai/gpt-4o-mini").unwrap();
    db1.set_model_display_name("openai/gpt-4o", "GPT-4o Live").unwrap();
    db1.disable_models("openai", &["gpt-3.5-turbo".to_string()]).unwrap();
    db1.save_custom_model(&json!({ "provider": "vllm", "id": "llama-3.3-70b", "type": "llm" }))
        .unwrap();
    assert_eq!(db1.load_model_aliases().unwrap().get("fast").map(String::as_str), Some("openai/gpt-4o-mini"));
    assert_eq!(db1.load_model_display_names().unwrap().get("openai/gpt-4o").map(String::as_str), Some("GPT-4o Live"));
    assert_eq!(db1.load_disabled_models().unwrap().get("openai").map(Vec::len), Some(1));
    assert_eq!(db1.load_custom_models().unwrap().len(), 1);

    // 7. Usage History + Request Details write-behind flush + Analytics
    for i in 0..25 {
        db1.queue_usage(UsageRecord {
            timestamp: Utc::now().to_rfc3339(),
            provider: Some("openai".to_string()),
            model: Some("gpt-4o".to_string()),
            connection_id: Some("pc-1".to_string()),
            api_key: Some(regen.key.clone()),
            api_key_id: Some(regen.id.clone()),
            endpoint: Some("/v1/chat/completions".to_string()),
            prompt_tokens: 100 + i,
            completion_tokens: 50 + i,
            cached_tokens: 10,
            cost: 0.012345678,
            status: "ok".to_string(),
            ..Default::default()
        });
        db1.record_request_detail(RequestDetail {
            id: format!("pg_req_{}_{}", nonce, i),
            timestamp: Utc::now().to_rfc3339(),
            provider: Some("openai".to_string()),
            model: Some("gpt-4o".to_string()),
            connection_id: Some("pc-1".to_string()),
            status: Some("ok".to_string()),
            latency_ms: Some(42),
            ..Default::default()
        });
    }
    db1.flush_write_behind().expect("flush write-behind");

    // Verify db2 sees usage & request details in shared Postgres
    assert_eq!(db2.get_usage_count().unwrap(), 25);
    assert_eq!(db2.get_request_details_count().unwrap(), 25);
    let stats = db2.get_usage_statistics("24h").unwrap();
    assert_eq!(stats["totalRequests"], 25);
    let chart = db2.get_chart_data("24h").unwrap();
    assert_eq!(chart.as_array().unwrap().len(), 24);
    let keys_usage = db2.get_api_keys_usage("24h").unwrap();
    assert_eq!(keys_usage.as_array().unwrap().len(), 1);
    let rd = db2.get_request_details(1, 10, Some("openai"), None, None).unwrap();
    assert_eq!(rd["total"], 25);
    assert_eq!(db2.get_recent_logs(5).len(), 5);

    // 8. Budget reserve + commit + double precision check
    let reserved = db1
        .debit_budget("pg-key-1", "req-b1", Some(10.0), None)
        .expect("reserve budget");
    assert_eq!(reserved["reserved"], true);
    let committed = db1
        .debit_budget("pg-key-1", "req-b1", None, Some(2.3456789))
        .expect("commit budget");
    assert_eq!(committed["committed"], true);

    // 9. Audit log + Metrics
    db1.append_audit_event(&json!({
        "actor": "admin",
        "action": "key.create",
        "resourceType": "apiKey",
        "resourceId": "pg-key-1",
        "ownerRef": "owner-42",
        "outcome": "success"
    }))
    .expect("append audit");
    let audits = db2.list_audit_events(Some("owner-42"), None, 10).unwrap();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0]["action"], "key.create");

    db1.record_metric_outcome(Some("t1"), "openai", "pc-1", "gpt-4o", true, 200, 35);
    let snap = db1.get_metrics_snapshot(Some("t1"));
    assert_eq!(snap["providers"].as_array().unwrap().len(), 1);
    assert!(db1.get_prometheus_metrics(&snap).contains("provider_successes_total"));

    // 10. Export & Import roundtrip into fresh schema
    let exported = db2.export_database().expect("export pg");
    assert_eq!(exported["apiKeys"].as_array().unwrap().len(), 1);
    assert_eq!(exported["combos"].as_array().unwrap().len(), 2);

    let import_dir = format!("/tmp/nr_pg_import_{}_{}", std::process::id(), nonce);
    let db3 = Database::open(&pg_config_for(&url, &import_dir)).expect("open db3");
    let counts = db3.import_database(&exported).expect("import into db3");
    assert_eq!(counts["apiKeys"], 1);
    assert_eq!(counts["combos"], 2);
    assert_eq!(counts["providerConnections"], 2);
    assert_eq!(db3.load_combos().unwrap().len(), 2);

    // Verify deletes work cleanly
    assert!(db1.delete_combo("tier-1").unwrap());
    assert!(db1.delete_provider_node("pn-1").unwrap());
    assert!(db1.delete_api_key("pg-key-1").unwrap());
    db1.delete_custom_model("vllm", "llama-3.3-70b", "llm").unwrap();
    db1.delete_model_alias("fast").unwrap();
    db1.delete_model_display_name("openai/gpt-4o").unwrap();
    db1.enable_models("openai", &["gpt-3.5-turbo".to_string()]).unwrap();
}

#[tokio::test]
async fn bench_live_postgres_vs_sqlite_wal() {
    let Some(url) = live_pg_url() else {
        eprintln!("[SKIP] TEST_DATABASE_URL / DATABASE_URL unset; skipping live Postgres vs SQLite benchmark");
        return;
    };

    let nonce = Utc::now().timestamp_micros();
    let sqlite_path = std::env::temp_dir().join(format!("nr_bench_wal_{}_{}.db", std::process::id(), nonce));
    let _ = std::fs::remove_file(&sqlite_path);

    let sqlite_db = Database::open_path(&sqlite_path).expect("open sqlite WAL");
    let pg_dir = format!("/tmp/nr_bench_pg_{}_{}", std::process::id(), nonce);
    let pg_db = Database::open(&pg_config_for(&url, &pg_dir)).expect("open live Postgres");

    // Seed identical dataset in both SQLite WAL and Live Postgres
    const NUM_KEYS: usize = 100;
    const NUM_CONNS: usize = 20;
    for i in 0..NUM_KEYS {
        let k = make_api_key(&format!("bk-{}", i), &format!("sk-bench-{:04}", i), Some("bench"));
        sqlite_db.save_api_key(&k).unwrap();
        pg_db.save_api_key(&k).unwrap();
    }
    for i in 0..NUM_CONNS {
        let prov = if i % 2 == 0 { "openai" } else { "anthropic" };
        let c = make_provider_conn(&format!("bc-{}", i), prov, i as i64, i % 5 != 0);
        sqlite_db.save_provider_connection(&c).unwrap();
        pg_db.save_provider_connection(&c).unwrap();
    }
    let combo = Combo {
        id: "bench-combo".to_string(),
        name: "bench-router".to_string(),
        kind: Some("fallback".to_string()),
        models: vec!["openai/gpt-4o".to_string(), "anthropic/claude-3-5-sonnet".to_string()],
        strategy: Some("fallback".to_string()),
        match_rules: Some("[]".to_string()),
        custom_prompt: Some("".to_string()),
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
    };
    sqlite_db.save_combo(&combo).unwrap();
    pg_db.save_combo(&combo).unwrap();

    // Warmup caches
    let _ = sqlite_db.get_api_key_by_key("sk-bench-0042").unwrap();
    let _ = pg_db.get_api_key_by_key("sk-bench-0042").unwrap();
    let _ = sqlite_db.get_active_provider_connections(Some("openai")).unwrap();
    let _ = pg_db.get_active_provider_connections(Some("openai")).unwrap();

    // 1. Key Lookups (Single-thread, 5,000 ops for SQLite WAL & PG Memory-Tiered; 500 ops for PG Direct Wire)
    const LOOKUP_ITERS: usize = 5_000;
    const WIRE_ITERS: usize = 500;

    let t0 = Instant::now();
    for i in 0..LOOKUP_ITERS {
        let key = format!("sk-bench-{:04}", i % NUM_KEYS);
        let found = sqlite_db.get_api_key_by_key(&key).unwrap();
        assert!(found.is_some());
    }
    let sqlite_lookup_el = t0.elapsed();

    let t1 = Instant::now();
    for i in 0..LOOKUP_ITERS {
        let key = format!("sk-bench-{:04}", i % NUM_KEYS);
        let found = pg_db.get_api_key_by_key(&key).unwrap();
        assert!(found.is_some());
    }
    let pg_cached_lookup_el = t1.elapsed();

    let pg_backend = pg_db.pg_backend().unwrap();
    let t2 = Instant::now();
    for i in 0..WIRE_ITERS {
        let key = format!("sk-bench-{:04}", i % NUM_KEYS);
        let found = pg_backend.query_api_key_by_key_uncached(&key).unwrap();
        assert!(found.is_some());
    }
    let pg_wire_lookup_el = t2.elapsed();

    // 2. Connection Routing (get_active_provider_connections + resolve_combo_chain)
    const ROUTE_ITERS: usize = 2_000;

    let t3 = Instant::now();
    for _ in 0..ROUTE_ITERS {
        let conns = sqlite_db.get_active_provider_connections(Some("openai")).unwrap();
        let chain = sqlite_db.resolve_combo_chain("bench-router");
        assert!(!conns.is_empty() && chain.len() == 2);
    }
    let sqlite_route_el = t3.elapsed();

    let t4 = Instant::now();
    for _ in 0..ROUTE_ITERS {
        let conns = pg_db.get_active_provider_connections(Some("openai")).unwrap();
        let chain = pg_db.resolve_combo_chain("bench-router");
        assert!(!conns.is_empty() && chain.len() == 2);
    }
    let pg_route_el = t4.elapsed();

    // 3. Usage Batch Writes (1,000 records)
    const USAGE_RECORDS: usize = 1_000;
    let records: Vec<UsageRecord> = (0..USAGE_RECORDS)
        .map(|i| UsageRecord {
            timestamp: Utc::now().to_rfc3339(),
            provider: Some("openai".to_string()),
            model: Some("gpt-4o".to_string()),
            connection_id: Some("bc-0".to_string()),
            api_key: Some("sk-bench-0001".to_string()),
            api_key_id: Some("bk-1".to_string()),
            endpoint: Some("/v1/chat/completions".to_string()),
            prompt_tokens: 100 + i as i64,
            completion_tokens: 50,
            cached_tokens: 0,
            cost: 0.002,
            status: "ok".to_string(),
            tokens: None,
            meta: None,
        })
        .collect();

    let t5 = Instant::now();
    for rec in &records {
        sqlite_db.queue_usage(rec.clone());
    }
    let sqlite_enqueue_el = t5.elapsed();
    // Wait for SQLite background worker (100ms tick + batch commits)
    let wait_start = Instant::now();
    while sqlite_db.get_usage_count().unwrap() < USAGE_RECORDS as i64
        && wait_start.elapsed() < Duration::from_secs(10)
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let sqlite_batch_total_el = t5.elapsed();
    assert_eq!(sqlite_db.get_usage_count().unwrap(), USAGE_RECORDS as i64);

    let t6 = Instant::now();
    for rec in &records {
        pg_db.queue_usage(rec.clone());
    }
    let pg_enqueue_el = t6.elapsed();
    pg_db.flush_write_behind().expect("flush PG usage batch");
    let pg_batch_total_el = t6.elapsed();
    assert_eq!(pg_db.get_usage_count().unwrap(), USAGE_RECORDS as i64);

    // 4. Concurrent Read Throughput (8 threads x 2,000 reads = 16,000 reads)
    const THREADS: usize = 8;
    const READS_PER_THREAD: usize = 2_000;
    let total_reads = THREADS * READS_PER_THREAD;

    let sqlite_counter = Arc::new(AtomicUsize::new(0));
    let t7 = Instant::now();
    let mut s_handles = Vec::with_capacity(THREADS);
    for t in 0..THREADS {
        let db = Arc::clone(&sqlite_db);
        let cnt = Arc::clone(&sqlite_counter);
        s_handles.push(std::thread::spawn(move || {
            for i in 0..READS_PER_THREAD {
                let k = format!("sk-bench-{:04}", (t * 13 + i) % NUM_KEYS);
                if db.get_api_key_by_key(&k).unwrap().is_some() {
                    cnt.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    for h in s_handles {
        h.join().unwrap();
    }
    let sqlite_conc_el = t7.elapsed();
    assert_eq!(sqlite_counter.load(Ordering::Relaxed), total_reads);

    let pg_counter = Arc::new(AtomicUsize::new(0));
    let t8 = Instant::now();
    let mut p_handles = Vec::with_capacity(THREADS);
    for t in 0..THREADS {
        let db = Arc::clone(&pg_db);
        let cnt = Arc::clone(&pg_counter);
        p_handles.push(std::thread::spawn(move || {
            for i in 0..READS_PER_THREAD {
                let k = format!("sk-bench-{:04}", (t * 13 + i) % NUM_KEYS);
                if db.get_api_key_by_key(&k).unwrap().is_some() {
                    cnt.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    for h in p_handles {
        h.join().unwrap();
    }
    let pg_conc_el = t8.elapsed();
    assert_eq!(pg_counter.load(Ordering::Relaxed), total_reads);

    // Concurrent direct TCP wire reads (8 threads x 200 reads = 1,600 live TCP queries)
    const WIRE_THREADS: usize = 8;
    const WIRE_READS_PER_THREAD: usize = 200;
    let wire_counter = Arc::new(AtomicUsize::new(0));
    let t9 = Instant::now();
    let mut w_handles = Vec::with_capacity(WIRE_THREADS);
    for t in 0..WIRE_THREADS {
        let db = Arc::clone(&pg_db);
        let cnt = Arc::clone(&wire_counter);
        w_handles.push(std::thread::spawn(move || {
            let backend = db.pg_backend().unwrap();
            for i in 0..WIRE_READS_PER_THREAD {
                let k = format!("sk-bench-{:04}", (t * 13 + i) % NUM_KEYS);
                if backend.query_api_key_by_key_uncached(&k).unwrap().is_some() {
                    cnt.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    for h in w_handles {
        h.join().unwrap();
    }
    let pg_wire_conc_el = t9.elapsed();
    assert_eq!(
        wire_counter.load(Ordering::Relaxed),
        WIRE_THREADS * WIRE_READS_PER_THREAD
    );

    let ops = |n: usize, d: Duration| -> f64 { n as f64 / d.as_secs_f64().max(1e-9) };
    let us_op = |n: usize, d: Duration| -> f64 { d.as_secs_f64() * 1_000_000.0 / n as f64 };

    println!("\n===========================================================================");
    println!(" 9Router Live Postgres vs SQLite WAL Benchmark (debug/release)");
    println!("===========================================================================");
    println!(
        "1. Key Lookup (1T):       SQLite WAL: {:>10.0} ops/s ({:>7.2} us/op) | PG Cache: {:>10.0} ops/s ({:>7.2} us/op) | PG Direct TCP: {:>8.0} ops/s ({:>7.2} us/op)",
        ops(LOOKUP_ITERS, sqlite_lookup_el),
        us_op(LOOKUP_ITERS, sqlite_lookup_el),
        ops(LOOKUP_ITERS, pg_cached_lookup_el),
        us_op(LOOKUP_ITERS, pg_cached_lookup_el),
        ops(WIRE_ITERS, pg_wire_lookup_el),
        us_op(WIRE_ITERS, pg_wire_lookup_el),
    );
    println!(
        "2. Connection Routing:    SQLite WAL: {:>10.0} ops/s ({:>7.2} us/op) | PG Cache: {:>10.0} ops/s ({:>7.2} us/op)",
        ops(ROUTE_ITERS, sqlite_route_el),
        us_op(ROUTE_ITERS, sqlite_route_el),
        ops(ROUTE_ITERS, pg_route_el),
        us_op(ROUTE_ITERS, pg_route_el),
    );
    println!(
        "3. Usage Batch (1k rows): SQLite WAL: {:.2}ms total ({:.0} rows/s, enqueue {:.2}ms) | PG Multi-Row: {:.2}ms total ({:.0} rows/s, enqueue {:.2}ms)",
        sqlite_batch_total_el.as_secs_f64() * 1000.0,
        ops(USAGE_RECORDS, sqlite_batch_total_el),
        sqlite_enqueue_el.as_secs_f64() * 1000.0,
        pg_batch_total_el.as_secs_f64() * 1000.0,
        ops(USAGE_RECORDS, pg_batch_total_el),
        pg_enqueue_el.as_secs_f64() * 1000.0,
    );
    println!(
        "4. Concurrent Reads (8T): SQLite WAL: {:>10.0} ops/s ({:.2}ms) | PG Cache: {:>10.0} ops/s ({:.2}ms) | PG Direct TCP: {:>8.0} ops/s ({:.2}ms)",
        ops(total_reads, sqlite_conc_el),
        sqlite_conc_el.as_secs_f64() * 1000.0,
        ops(total_reads, pg_conc_el),
        pg_conc_el.as_secs_f64() * 1000.0,
        ops(WIRE_THREADS * WIRE_READS_PER_THREAD, pg_wire_conc_el),
        pg_wire_conc_el.as_secs_f64() * 1000.0,
    );
    println!("===========================================================================\n");

    drop(sqlite_db);
    drop(pg_db);
    let _ = std::fs::remove_file(&sqlite_path);
    let _ = std::fs::remove_file(sqlite_path.with_extension("db-wal"));
    let _ = std::fs::remove_file(sqlite_path.with_extension("db-shm"));
}

#[tokio::test]
async fn test_pg_listen_notify_rate_limit_hot_reload_across_replicas() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use ninerouter::auth::create_dashboard_jwt;
    use ninerouter::cache::TokenCache;
    use ninerouter::combo_router::ComboTracker;
    use ninerouter::original::Original;
    use ninerouter::proxy::ProxyService;
    use ninerouter::ratelimit::RateLimiter;
    use ninerouter::routes::{create_router, AppState, ConsoleLogBuffer};
    use ninerouter::translator::{ChatCompletionRequest, ChatMessage};
    use tower::ServiceExt;

    let Some(url) = live_pg_url() else {
        eprintln!(
            "[SKIP] TEST_DATABASE_URL / DATABASE_URL unset; skipping live Postgres LISTEN/NOTIFY rateLimit sync test"
        );
        return;
    };

    let nonce = Utc::now().timestamp_micros();
    let shared_dir = format!("/tmp/nr_pg_rl_sync_{}_{}", std::process::id(), nonce);
    let mut cfg = pg_config_for(&url, &shared_dir);
    cfg.jwt_secret = format!("test-jwt-secret-{}", nonce);
    let cfg = Arc::new(cfg);

    let db_a = Database::open(&cfg).expect("open Postgres replica A");
    let key = make_api_key("rl-sync-key", &format!("sk-rl-sync-{}", nonce), None);
    db_a.save_api_key(&key).expect("save shared API key");

    // Open replica B against the same DATA_DIR (shared Postgres schema & LISTEN/NOTIFY channel)
    let db_b = Database::open(&cfg).expect("open Postgres replica B");

    let make_state = |db: Arc<Database>| -> (AppState, Arc<RateLimiter>) {
        let combo_tracker = ComboTracker::new();
        let token_cache = TokenCache::default();
        let chat_req = ChatCompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage::user("ping")],
            ..Default::default()
        };
        token_cache.set_json(
            &token_cache.compute_chat_hash(&chat_req),
            axum::body::Bytes::from_static(
                b"{\"id\":\"chatcmpl-pg-sync\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"pong\"}}]}",
            ),
        );
        let rate_limiter = Arc::new(RateLimiter::from_config(&cfg));
        let state = AppState {
            config: Arc::clone(&cfg),
            db,
            proxy: Arc::new(ProxyService::new()),
            original: Original::empty(),
            combo_tracker: Arc::clone(&combo_tracker),
            token_cache,
            circuit_breaker: Arc::clone(&combo_tracker.circuit_breakers),
            console_buffer: ConsoleLogBuffer::new(),
            rate_limiter: Arc::clone(&rate_limiter),
        };
        (state, rate_limiter)
    };

    let (state_a, _rl_a) = make_state(Arc::clone(&db_a));
    let (state_b, rl_b) = make_state(Arc::clone(&db_b));

    let app_a = create_router(state_a);
    let app_b = create_router(state_b);

    // Give replica B's background LISTEN connection a brief moment to complete `LISTEN`.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 1. Before update: replica B has rate limiting disabled -> 3 requests from same IP succeed.
    for _ in 0..3 {
        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", key.key))
            .header("Content-Type", "application/json")
            .header("x-forwarded-for", "203.0.113.88")
            .body(Body::from(
                r#"{"model":"gpt-4o","messages":[{"role":"user","content":"ping"}]}"#,
            ))
            .unwrap();
        let resp = app_b.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // 2. Instance A updates rateLimit settings via PATCH /api/settings.
    let jwt = create_dashboard_jwt(&cfg.jwt_secret);
    let auth_cookie = format!("auth_token={}", jwt);
    let t_patch = Instant::now();
    let patch_req = Request::builder()
        .method("PATCH")
        .uri("/api/settings")
        .header("Cookie", &auth_cookie)
        .header("Content-Type", "application/json")
        .body(Body::from(
            serde_json::to_string(&json!({
                "rateLimit": {
                    "ip": {
                        "enabled": true,
                        "rpm": 1,
                        "rps": null,
                        "windowSecs": 60,
                        "allowlist": []
                    }
                }
            }))
            .unwrap(),
        ))
        .unwrap();
    let patch_resp = app_a.clone().oneshot(patch_req).await.unwrap();
    assert_eq!(patch_resp.status(), StatusCode::OK);

    // 3. Wait for LISTEN/NOTIFY -> Instance B settings resync -> RateLimiter hot-reload.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut synced = false;
    while Instant::now() < deadline {
        let rl_json = rl_b.to_settings_json();
        if rl_json["ip"]["enabled"] == json!(true) && rl_json["ip"]["rpm"] == json!(1) {
            synced = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let sync_latency = t_patch.elapsed();
    assert!(
        synced,
        "Replica B RateLimiter did not hot-reload via LISTEN/NOTIFY within 5s: {:?}",
        rl_b.to_settings_json()
    );
    println!(
        "[PG LISTEN/NOTIFY] rateLimit hot-reload sync latency (Instance A PATCH -> Instance B RateLimiter): {:.2}ms",
        sync_latency.as_secs_f64() * 1000.0
    );

    // 4. Without restarting Instance B, verify HTTP 429 is enforced on a fresh IP.
    let req_1 = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", key.key))
        .header("Content-Type", "application/json")
        .header("x-forwarded-for", "203.0.113.99")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"ping"}]}"#,
        ))
        .unwrap();
    assert_eq!(app_b.clone().oneshot(req_1).await.unwrap().status(), StatusCode::OK);

    let req_2 = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", key.key))
        .header("Content-Type", "application/json")
        .header("x-forwarded-for", "203.0.113.99")
        .body(Body::from(
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"ping"}]}"#,
        ))
        .unwrap();
    let resp_2 = app_b.clone().oneshot(req_2).await.unwrap();
    assert_eq!(
        resp_2.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "Replica B must enforce HTTP 429 after receiving LISTEN/NOTIFY without restart"
    );

    drop(app_a);
    drop(app_b);
    drop(db_a);
    drop(db_b);
    let _ = std::fs::remove_dir_all(&shared_dir);
}
