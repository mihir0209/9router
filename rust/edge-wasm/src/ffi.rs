use std::cell::RefCell;
use std::sync::{Arc, OnceLock};
use serde_json::Value;

use crate::coalescer::{CoalescerConfig, CoordinatorCoalescer};
use crate::coordinator::{GatewayCoordinator, OutcomeItem};
use crate::router::{ApiKeyRecord, ComboConfig, FastRouteRequest, FastRouteResolver, ProviderConnection};

// Thread-safe singletons for edge WASM instance
static COORDINATOR: OnceLock<Arc<GatewayCoordinator>> = OnceLock::new();
static COALESCER: OnceLock<Arc<CoordinatorCoalescer>> = OnceLock::new();
static ROUTER: OnceLock<Arc<FastRouteResolver>> = OnceLock::new();

thread_local! {
    static LAST_RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn get_coordinator() -> &'static Arc<GatewayCoordinator> {
    COORDINATOR.get_or_init(|| Arc::new(GatewayCoordinator::new()))
}

fn get_coalescer() -> &'static Arc<CoordinatorCoalescer> {
    COALESCER.get_or_init(|| {
        let coord = get_coordinator().clone();
        Arc::new(CoordinatorCoalescer::new(coord, CoalescerConfig::default()))
    })
}

fn get_router() -> &'static Arc<FastRouteResolver> {
    ROUTER.get_or_init(|| {
        let coord = get_coordinator().clone();
        Arc::new(FastRouteResolver::new(coord))
    })
}

fn set_result_bytes(bytes: Vec<u8>) -> u64 {
    let len = bytes.len() as u32;
    let ptr = bytes.as_ptr() as u32;
    LAST_RESULT.with(|lr| {
        *lr.borrow_mut() = bytes;
    });
    ((len as u64) << 32) | (ptr as u64)
}

fn set_result_json<T: serde::Serialize>(val: &T) -> u64 {
    let bytes = serde_json::to_vec(val).unwrap_or_default();
    set_result_bytes(bytes)
}

fn str_from_ptr<'a>(ptr: *const u8, len: usize) -> &'a str {
    if ptr.is_null() || len == 0 {
        return "";
    }
    unsafe {
        let slice = std::slice::from_raw_parts(ptr, len);
        std::str::from_utf8(slice).unwrap_or("")
    }
}

// ----------------------------------------------------------------------------
// Memory Management Exports
// ----------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn wasm_alloc(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn wasm_dealloc(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn edge_wasm_get_result_ptr() -> *const u8 {
    LAST_RESULT.with(|lr| lr.borrow().as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn edge_wasm_get_result_len() -> usize {
    LAST_RESULT.with(|lr| lr.borrow().len())
}

#[unsafe(no_mangle)]
pub extern "C" fn edge_wasm_init() -> u32 {
    let _ = get_coordinator();
    let _ = get_coalescer();
    let _ = get_router();
    1
}

// ----------------------------------------------------------------------------
// JSON-RPC Dispatch Interface
// ----------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn edge_wasm_rpc(
    service_id: u32,
    method_ptr: *const u8,
    method_len: usize,
    payload_ptr: *const u8,
    payload_len: usize,
) -> u64 {
    let method = str_from_ptr(method_ptr, method_len);
    let payload_str = str_from_ptr(payload_ptr, payload_len);
    let payload: Value = serde_json::from_str(payload_str).unwrap_or(Value::Null);

    match service_id {
        // 1 = GatewayCoordinator
        1 => dispatch_coordinator(method, &payload),
        // 2 = CoordinatorCoalescer
        2 => dispatch_coalescer(method, &payload),
        // 3 = FastRouteResolver
        3 => dispatch_router(method, &payload),
        _ => set_result_bytes(b"{\"error\":\"unknown_service\"}".to_vec()),
    }
}

fn dispatch_coordinator(method: &str, payload: &Value) -> u64 {
    let coord = get_coordinator();
    let now = payload
        .get("now")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    match method {
        "checkRateLimit" => {
            let key_id = payload.get("keyId").and_then(|v| v.as_str()).unwrap_or("");
            let limit = payload.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            let window_ms = payload.get("windowMs").and_then(|v| v.as_u64()).unwrap_or(60_000);
            let res = coord.check_rate_limit(key_id, limit, window_ms, now);
            set_result_json(&res)
        }
        "peekRateLimit" => {
            let key_id = payload.get("keyId").and_then(|v| v.as_str()).unwrap_or("");
            let limit = payload.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            let window_ms = payload.get("windowMs").and_then(|v| v.as_u64()).unwrap_or(60_000);
            let res = coord.peek_rate_limit(key_id, limit, window_ms, now);
            set_result_json(&res)
        }
        "beginRequest" => {
            let key_id = payload.get("keyId").and_then(|v| v.as_str()).unwrap_or("");
            let max_concurrent = payload
                .get("maxConcurrent")
                .and_then(|v| v.as_u64())
                .unwrap_or(10) as usize;
            let res = coord.begin_request(key_id, max_concurrent, now);
            set_result_json(&res)
        }
        "renewRequest" => {
            let key_id = payload.get("keyId").and_then(|v| v.as_str()).unwrap_or("");
            let lease_id = payload.get("leaseId").and_then(|v| v.as_str()).unwrap_or("");
            let ttl_ms = payload.get("ttlMs").and_then(|v| v.as_u64()).unwrap_or(120_000);
            let res = coord.renew_request(key_id, lease_id, ttl_ms, now);
            set_result_json(&res)
        }
        "endRequest" => {
            let key_id = payload.get("keyId").and_then(|v| v.as_str()).unwrap_or("");
            let lease_id = payload.get("leaseId").and_then(|v| v.as_str()).unwrap_or("");
            let active = coord.end_request(key_id, lease_id);
            set_result_json(&serde_json::json!({ "active": active }))
        }
        "setLock" => {
            let provider = payload.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let connection_id = payload.get("connectionId").and_then(|v| v.as_str()).unwrap_or("");
            let model = payload.get("model").and_then(|v| v.as_str());
            let until_ms = payload.get("untilMs").and_then(|v| v.as_u64()).unwrap_or(0);
            let status = payload.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let reason = payload.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            let res = coord.set_lock(provider, connection_id, model, until_ms, status, reason, now);
            set_result_json(&res)
        }
        "clearLock" => {
            let provider = payload.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let connection_id = payload.get("connectionId").and_then(|v| v.as_str()).unwrap_or("");
            let model = payload.get("model").and_then(|v| v.as_str());
            let exp = payload.get("expectedGeneration").and_then(|v| v.as_u64());
            let res = coord.clear_lock(provider, connection_id, model, exp);
            set_result_json(&res)
        }
        "getActiveLocks" => {
            let provider = payload.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let cids: Vec<String> = payload
                .get("connectionIds")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let model = payload.get("model").and_then(|v| v.as_str());
            let res = coord.get_active_locks(provider, &cids, model, now);
            set_result_json(&res)
        }
        "recordOutcome" => {
            let provider = payload.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let connection_id = payload.get("connectionId").and_then(|v| v.as_str()).unwrap_or("");
            let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let success = payload.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
            let latency_ms = payload.get("latencyMs").and_then(|v| v.as_f64());
            let status = payload.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let cost_usd = payload.get("costUsd").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let res = coord.record_outcome(provider, connection_id, model, success, latency_ms, status, cost_usd, now);
            set_result_json(&res)
        }
        "recordOutcomes" => {
            let items: Vec<OutcomeItem> = payload
                .get("items")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let recorded = coord.record_outcomes(&items, now);
            set_result_json(&serde_json::json!({ "recorded": recorded }))
        }
        "getScores" => {
            let provider = payload.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let cids: Vec<String> = payload
                .get("connectionIds")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let res = coord.get_scores(provider, &cids, model, now);
            set_result_json(&res)
        }
        "reserveBudget" => {
            let key_id = payload.get("keyId").and_then(|v| v.as_str()).unwrap_or("");
            let request_id = payload.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
            let amount = payload.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let expires_at = payload.get("expiresAt").and_then(|v| v.as_u64()).unwrap_or(0);
            let avail = payload.get("availableBudget").and_then(|v| v.as_f64());
            let res = coord.reserve_budget(key_id, request_id, amount, expires_at, avail, now);
            set_result_json(&res)
        }
        "releaseBudget" => {
            let key_id = payload.get("keyId").and_then(|v| v.as_str()).unwrap_or("");
            let request_id = payload.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
            let res = coord.release_budget(key_id, request_id);
            set_result_json(&res)
        }
        "bumpVersion" => {
            let domain = payload.get("domain").and_then(|v| v.as_str()).unwrap_or("");
            let ver = coord.bump_version(domain);
            set_result_json(&serde_json::json!({ "version": ver }))
        }
        "getVersions" => {
            let domains: Vec<String> = payload
                .get("domains")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let res = coord.get_versions(&domains);
            set_result_json(&res)
        }
        _ => set_result_bytes(b"{\"error\":\"unknown_method\"}".to_vec()),
    }
}

fn dispatch_coalescer(method: &str, payload: &Value) -> u64 {
    let coalescer = get_coalescer();
    let now = payload.get("now").and_then(|v| v.as_u64()).unwrap_or(0);

    match method {
        "readCached" => {
            let key = payload.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let val = coalescer.get_cached(key, now);
            set_result_json(&val)
        }
        "setCached" => {
            let key = payload.get("key").and_then(|v| v.as_str()).unwrap_or("");
            let scope = payload.get("scope").and_then(|v| v.as_str()).unwrap_or("");
            let val = payload.get("value").cloned().unwrap_or(Value::Null);
            let ttl = payload.get("ttlMs").and_then(|v| v.as_u64());
            coalescer.set_cached(key, scope, val, now, ttl);
            set_result_bytes(b"{\"ok\":true}".to_vec())
        }
        "invalidate" => {
            let scope = payload.get("scope").and_then(|v| v.as_str()).unwrap_or("");
            coalescer.invalidate(scope);
            set_result_bytes(b"{\"ok\":true}".to_vec())
        }
        "queueOutcome" => {
            let provider = payload.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let connection_id = payload.get("connectionId").and_then(|v| v.as_str()).unwrap_or("");
            let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let success = payload.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
            let latency_ms = payload.get("latencyMs").and_then(|v| v.as_f64());
            let status = payload.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let cost_usd = payload.get("costUsd").and_then(|v| v.as_f64()).unwrap_or(0.0);
            coalescer.queue_outcome(provider, connection_id, model, success, latency_ms, status, cost_usd, now);
            set_result_bytes(b"{\"queued\":true}".to_vec())
        }
        "queueMetrics" => {
            let tenant = payload.get("tenant").and_then(|v| v.as_str());
            let provider = payload.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let connection_id = payload.get("connectionId").and_then(|v| v.as_str()).unwrap_or("");
            let model = payload.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let success = payload.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
            let status = payload.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let latency_ms = payload.get("latencyMs").and_then(|v| v.as_f64());
            coalescer.queue_metrics(tenant, provider, connection_id, model, success, status, latency_ms, now);
            set_result_bytes(b"{\"queued\":true}".to_vec())
        }
        "flush" => {
            let total = coalescer.flush(now);
            set_result_json(&serde_json::json!({ "flushed": total }))
        }
        "pendingCounts" => {
            let (events, outcomes, metrics) = coalescer.pending_counts();
            set_result_json(&serde_json::json!({
                "events": events,
                "outcomes": outcomes,
                "metrics": metrics,
            }))
        }
        _ => set_result_bytes(b"{\"error\":\"unknown_method\"}".to_vec()),
    }
}

fn dispatch_router(method: &str, payload: &Value) -> u64 {
    let router = get_router();
    let now = payload.get("now").and_then(|v| v.as_u64()).unwrap_or(0);

    match method {
        "configure" => {
            if let Some(keys) = payload.get("apiKeys").and_then(|v| serde_json::from_value::<Vec<ApiKeyRecord>>(v.clone()).ok()) {
                router.set_api_keys(keys);
            }
            if let Some(aliases) = payload.get("aliases").and_then(|v| serde_json::from_value::<std::collections::HashMap<String, String>>(v.clone()).ok()) {
                router.set_aliases(aliases);
            }
            if let Some(combos) = payload.get("combos").and_then(|v| serde_json::from_value::<Vec<ComboConfig>>(v.clone()).ok()) {
                router.set_combos(combos);
            }
            if let Some(conns) = payload.get("connections").and_then(|v| serde_json::from_value::<Vec<ProviderConnection>>(v.clone()).ok()) {
                router.set_connections(conns);
            }
            set_result_bytes(b"{\"configured\":true}".to_vec())
        }
        "resolveRoute" => {
            let req: FastRouteRequest = serde_json::from_value(payload.clone()).unwrap_or(FastRouteRequest {
                model: String::new(),
                api_key: None,
                path: None,
            });
            let res = router.resolve(&req, now);
            set_result_json(&res)
        }
        _ => set_result_bytes(b"{\"error\":\"unknown_method\"}".to_vec()),
    }
}

// ----------------------------------------------------------------------------
// Zero-Allocation High-Speed Scalar FFI
// ----------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn edge_fast_check_rate_limit(
    key_ptr: *const u8,
    key_len: usize,
    limit: u32,
    window_ms: u32,
    now_low: u32,
    now_high: u32,
) -> u64 {
    let key = str_from_ptr(key_ptr, key_len);
    let now = ((now_high as u64) << 32) | (now_low as u64);
    let res = get_coordinator().check_rate_limit(key, limit as usize, window_ms as u64, now);
    let allowed_bit = if res.allowed { 1u64 } else { 0u64 };
    let remaining_part = (res.remaining as u64) & 0x7FFFFFFF;
    let reset_delta = res.reset_at.saturating_sub(now) as u32;

    (allowed_bit << 63) | ((reset_delta as u64) << 32) | remaining_part
}

#[unsafe(no_mangle)]
pub extern "C" fn edge_fast_begin_request(
    key_ptr: *const u8,
    key_len: usize,
    max_concurrent: u32,
    now_low: u32,
    now_high: u32,
) -> u64 {
    let key = str_from_ptr(key_ptr, key_len);
    let now = ((now_high as u64) << 32) | (now_low as u64);
    let res = get_coordinator().begin_request(key, max_concurrent as usize, now);
    let allowed_bit = if res.allowed { 1u64 } else { 0u64 };
    let active_part = (res.active as u64) & 0x7FFFFFFF;

    if let Some(ref lid) = res.lease_id {
        set_result_bytes(lid.as_bytes().to_vec());
    }

    (allowed_bit << 63) | active_part
}

#[unsafe(no_mangle)]
pub extern "C" fn edge_fast_end_request(
    key_ptr: *const u8,
    key_len: usize,
    lease_ptr: *const u8,
    lease_len: usize,
) -> u32 {
    let key = str_from_ptr(key_ptr, key_len);
    let lease = str_from_ptr(lease_ptr, lease_len);
    get_coordinator().end_request(key, lease) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn edge_fast_clear_lock(
    prov_ptr: *const u8,
    prov_len: usize,
    conn_ptr: *const u8,
    conn_len: usize,
    model_ptr: *const u8,
    model_len: usize,
    expected_gen: i64,
) -> u32 {
    let prov = str_from_ptr(prov_ptr, prov_len);
    let conn = str_from_ptr(conn_ptr, conn_len);
    let model = if model_len > 0 { Some(str_from_ptr(model_ptr, model_len)) } else { None };
    let exp = if expected_gen >= 0 { Some(expected_gen as u64) } else { None };
    let res = get_coordinator().clear_lock(prov, conn, model, exp);
    if res.cleared { 1 } else { 0 }
}
