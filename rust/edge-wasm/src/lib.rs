pub mod coordinator;
pub mod coalescer;
pub mod router;
pub mod ffi;

pub use coordinator::*;
pub use coalescer::*;
pub use router::*;
pub use ffi::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn test_coordinator_rate_limiting() {
        let coord = GatewayCoordinator::new();
        let now = 1_000_000;

        // 3 requests allowed with limit 3
        let r1 = coord.check_rate_limit("user-1", 3, 60_000, now);
        assert!(r1.allowed);
        assert_eq!(r1.count, 1);
        assert_eq!(r1.remaining, 2);

        let r2 = coord.check_rate_limit("user-1", 3, 60_000, now + 100);
        assert!(r2.allowed);
        assert_eq!(r2.count, 2);
        assert_eq!(r2.remaining, 1);

        let r3 = coord.check_rate_limit("user-1", 3, 60_000, now + 200);
        assert!(r3.allowed);
        assert_eq!(r3.count, 3);
        assert_eq!(r3.remaining, 0);

        // 4th request rejected
        let r4 = coord.check_rate_limit("user-1", 3, 60_000, now + 300);
        assert!(!r4.allowed);
        assert_eq!(r4.count, 4);
        assert_eq!(r4.remaining, 0);

        // Peek does not increment
        let peek = coord.peek_rate_limit("user-1", 3, 60_000, now + 350);
        assert_eq!(peek.count, 4);
        assert_eq!(peek.remaining, 0);

        // After window expires, counter resets
        let r5 = coord.check_rate_limit("user-1", 3, 60_000, now + 60_001);
        assert!(r5.allowed);
        assert_eq!(r5.count, 1);
        assert_eq!(r5.remaining, 2);
    }

    #[test]
    fn test_coordinator_concurrency_leases() {
        let coord = GatewayCoordinator::new();
        let now = 1_000_000;

        // Begin request with max 2
        let l1 = coord.begin_request("key-1", 2, now);
        assert!(l1.allowed);
        assert_eq!(l1.active, 1);
        let lease1 = l1.lease_id.unwrap();

        let l2 = coord.begin_request("key-1", 2, now + 10);
        assert!(l2.allowed);
        assert_eq!(l2.active, 2);
        let lease2 = l2.lease_id.unwrap();

        // 3rd rejected
        let l3 = coord.begin_request("key-1", 2, now + 20);
        assert!(!l3.allowed);
        assert_eq!(l3.active, 2);
        assert!(l3.lease_id.is_none());

        // Renew lease 1
        let renew = coord.renew_request("key-1", &lease1, 60_000, now + 50);
        assert!(renew.renewed);
        assert_eq!(renew.expires_at, now + 50 + 60_000);

        // End lease 1
        let remaining = coord.end_request("key-1", &lease1);
        assert_eq!(remaining, 1);

        // Now new lease can be acquired
        let l4 = coord.begin_request("key-1", 2, now + 100);
        assert!(l4.allowed);
        assert_eq!(l4.active, 2);

        coord.end_request("key-1", &lease2);
    }

    #[test]
    fn test_coordinator_locks_with_generation() {
        let coord = GatewayCoordinator::new();
        let now = 1_000_000;

        // Set lock
        let lock1 = coord.set_lock(
            "openai",
            "conn-1",
            Some("gpt-4o"),
            now + 30_000,
            429,
            "rate_limited",
            now,
        );
        assert!(lock1.locked);
        assert_eq!(lock1.generation, 1);

        // Active locks query
        let active = coord.get_active_locks(
            "openai",
            &["conn-1".to_string(), "conn-2".to_string()],
            Some("gpt-4o"),
            now + 1_000,
        );
        assert_eq!(active.locks.len(), 1);
        assert_eq!(active.generation, 1);
        assert_eq!(active.locks[0].reason, "rate_limited");

        // Set newer lock
        let lock2 = coord.set_lock(
            "openai",
            "conn-1",
            Some("gpt-4o"),
            now + 60_000,
            503,
            "overloaded",
            now + 2_000,
        );
        assert_eq!(lock2.generation, 2);

        // Attempting to clear with stale generation 1 fails
        let clear_stale = coord.clear_lock("openai", "conn-1", Some("gpt-4o"), Some(1));
        assert!(!clear_stale.cleared);
        assert_eq!(clear_stale.reason.as_deref(), Some("newer_lock"));

        // Clear with valid generation 2 succeeds
        let clear_valid = coord.clear_lock("openai", "conn-1", Some("gpt-4o"), Some(2));
        assert!(clear_valid.cleared);

        let active_after = coord.get_active_locks(
            "openai",
            &["conn-1".to_string()],
            Some("gpt-4o"),
            now + 3_000,
        );
        assert_eq!(active_after.locks.len(), 0);
    }

    #[test]
    fn test_coalescer_caching_and_batching() {
        let coord = Arc::new(GatewayCoordinator::new());
        let coalescer = CoordinatorCoalescer::new(coord.clone(), CoalescerConfig::default());
        let now = 1_000_000;

        // Read cached
        let mut load_count = 0;
        let v1 = coalescer.read_cached("test-key", "scope-1", now, || {
            load_count += 1;
            serde_json::json!({ "foo": "bar" })
        });
        assert_eq!(load_count, 1);
        assert_eq!(v1["foo"], "bar");

        let v2 = coalescer.read_cached("test-key", "scope-1", now + 1_000, || {
            load_count += 1;
            serde_json::json!({ "foo": "baz" })
        });
        assert_eq!(load_count, 1); // Served from cache!
        assert_eq!(v2["foo"], "bar");

        // Invalidate scope
        coalescer.invalidate("scope-1");
        let v3 = coalescer.read_cached("test-key", "scope-1", now + 2_000, || {
            load_count += 1;
            serde_json::json!({ "foo": "refreshed" })
        });
        assert_eq!(load_count, 2);
        assert_eq!(v3["foo"], "refreshed");

        // Queue outcomes
        coalescer.queue_outcome("anthropic", "conn-a", "claude-3-5", true, Some(120.0), 200, 0.005, now);
        coalescer.queue_outcome("anthropic", "conn-a", "claude-3-5", true, Some(100.0), 200, 0.005, now + 10);
        let counts = coalescer.pending_counts();
        assert_eq!(counts.0, 2); // 2 events
        assert_eq!(counts.1, 1); // 1 coalesced outcome item

        // Flush to coordinator
        let flushed = coalescer.flush(now + 20);
        assert_eq!(flushed, 1);

        let scores = coord.get_scores("anthropic", &["conn-a".to_string()], "claude-3-5", now + 30);
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].samples, 2);
        assert_eq!(scores[0].successes, 2);
    }

    #[test]
    fn test_fast_route_resolution_with_combos_and_aliases() {
        let coord = Arc::new(GatewayCoordinator::new());
        let router = FastRouteResolver::new(coord.clone());
        let now = 1_000_000;

        // Configure router
        router.set_api_keys(vec![ApiKeyRecord {
            id: "key-1".to_string(),
            key: "sk-test-123".to_string(),
            rate_limit: Some(100),
            rate_limit_window_ms: Some(60_000),
            max_concurrent: Some(10),
            allowed_models: vec!["*".to_string()],
            is_active: true,
        }]);

        let mut aliases = HashMap::new();
        aliases.insert("claude".to_string(), "claude-3-7-sonnet".to_string());
        aliases.insert("smart".to_string(), "combo-smart".to_string());
        router.set_aliases(aliases);

        router.set_combos(vec![
            ComboConfig {
                name: "combo-smart".to_string(),
                strategy: ComboStrategy::RoundRobin,
                models: vec!["gpt-4o".to_string(), "claude-3-7-sonnet".to_string()],
            },
        ]);

        router.set_connections(vec![
            ProviderConnection {
                provider: "openai".to_string(),
                connection_id: "conn-openai-1".to_string(),
                model: "gpt-4o".to_string(),
                priority: 1,
                weight: 100,
                is_active: true,
                base_url: Some("https://api.openai.com/v1".to_string()),
            },
            ProviderConnection {
                provider: "anthropic".to_string(),
                connection_id: "conn-anthropic-1".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                priority: 1,
                weight: 100,
                is_active: true,
                base_url: Some("https://api.anthropic.com".to_string()),
            },
        ]);

        // 1. Direct model with valid key
        let res1 = router.resolve(
            &FastRouteRequest {
                model: "gpt-4o".to_string(),
                api_key: Some("sk-test-123".to_string()),
                path: Some("/v1/chat/completions".to_string()),
            },
            now,
        );
        assert!(res1.allowed);
        assert_eq!(res1.status, 200);
        assert_eq!(res1.resolved_model, "gpt-4o");
        assert_eq!(res1.selected_provider, "openai");
        assert_eq!(res1.selected_connection_id, "conn-openai-1");
        assert!(res1.lease_id.is_some());

        // 2. Alias resolution: "claude" -> "claude-3-7-sonnet"
        let res2 = router.resolve(
            &FastRouteRequest {
                model: "claude".to_string(),
                api_key: Some("sk-test-123".to_string()),
                path: Some("/v1/chat/completions".to_string()),
            },
            now + 10,
        );
        assert!(res2.allowed);
        assert_eq!(res2.resolved_model, "claude-3-7-sonnet");
        assert_eq!(res2.selected_provider, "anthropic");

        // 3. Combo round-robin: "smart" -> "combo-smart" -> alternates between gpt-4o and claude
        let res3 = router.resolve(
            &FastRouteRequest {
                model: "smart".to_string(),
                api_key: Some("sk-test-123".to_string()),
                path: Some("/v1/chat/completions".to_string()),
            },
            now + 20,
        );
        assert_eq!(res3.resolved_model, "gpt-4o");

        let res4 = router.resolve(
            &FastRouteRequest {
                model: "smart".to_string(),
                api_key: Some("sk-test-123".to_string()),
                path: Some("/v1/chat/completions".to_string()),
            },
            now + 30,
        );
        assert_eq!(res4.resolved_model, "claude-3-7-sonnet");

        // 4. Lock provider on gpt-4o, verify combo automatically diverts to claude
        coord.set_lock("openai", "conn-openai-1", Some("gpt-4o"), now + 10_000, 429, "rate_limited", now);
        let res5 = router.resolve(
            &FastRouteRequest {
                model: "smart".to_string(),
                api_key: Some("sk-test-123".to_string()),
                path: Some("/v1/chat/completions".to_string()),
            },
            now + 40,
        );
        assert_eq!(res5.resolved_model, "claude-3-7-sonnet");
    }
}
