use std::sync::Arc;
use std::time::Instant;
use ninerouter::ratelimit::RateLimiter;
use ninerouter::config::{ModelLimitConfig, RateLimitConfig};

#[test]
fn test_rate_limiter_sub_microsecond_overhead_benchmark() {
    println!("\n========================================================");
    println!(" 9Router Sliding Window Rate Limiter Performance Benchmark");
    println!("========================================================");

    let mut cfg = RateLimitConfig::default();
    cfg.ip_rate_limit_enabled = true;
    cfg.ip_rpm = Some(10_000_000); // High quota so we measure throughput without throttling
    cfg.model_rate_limit_enabled = true;
    cfg.model_default_rpm = Some(10_000_000);
    cfg.model_default_tpm = Some(100_000_000);

    let mut limits = std::collections::HashMap::new();
    limits.insert(
        "gpt-4o".to_string(),
        ModelLimitConfig {
            rpm: Some(10_000_000),
            tpm: Some(100_000_000),
        },
    );
    limits.insert(
        "claude-3-7-sonnet".to_string(),
        ModelLimitConfig {
            rpm: Some(10_000_000),
            tpm: Some(100_000_000),
        },
    );
    cfg.model_limits = limits;

    let limiter = Arc::new(RateLimiter::new(cfg));

    // Warmup
    for i in 0..10_000 {
        let ip = format!("192.168.1.{}", i % 256);
        let _ = limiter.check_ip(&ip);
        let _ = limiter.check_model("gpt-4o", 10);
    }
    limiter.reset_all();

    // 1. Single-Threaded Client IP Benchmark
    const IP_ITERS: usize = 500_000;
    let ips: Vec<String> = (0..256).map(|i| format!("192.168.1.{}", i)).collect();

    let start = Instant::now();
    for i in 0..IP_ITERS {
        let ip = &ips[i % ips.len()];
        let _ = limiter.check_ip(ip);
    }
    let elapsed = start.elapsed();
    let nanos_per_op = elapsed.as_nanos() as f64 / IP_ITERS as f64;
    let ops_per_sec = (IP_ITERS as f64 / elapsed.as_secs_f64()) as u64;

    let max_allowed = if cfg!(debug_assertions) { 2500.0 } else { 1000.0 };

    println!("[Single-Thread] check_ip:");
    println!("  Iterations:      {}", IP_ITERS);
    println!("  Total Time:      {:?}", elapsed);
    println!("  Latency / Op:    {:.2} ns ({:.4} µs)", nanos_per_op, nanos_per_op / 1000.0);
    println!("  Throughput:      {} ops/sec", ops_per_sec);
    assert!(
        nanos_per_op < max_allowed,
        "check_ip overhead must be within threshold (<{:.0}ns), got {:.2}ns",
        max_allowed,
        nanos_per_op
    );

    // 2. Single-Threaded Model Quota Benchmark (RPM + TPM)
    const MODEL_ITERS: usize = 500_000;
    let models = ["gpt-4o", "claude-3-7-sonnet", "deepseek-r1"];

    let start = Instant::now();
    for i in 0..MODEL_ITERS {
        let model = models[i % models.len()];
        let _ = limiter.check_model(model, 15);
    }
    let elapsed = start.elapsed();
    let nanos_per_op = elapsed.as_nanos() as f64 / MODEL_ITERS as f64;
    let ops_per_sec = (MODEL_ITERS as f64 / elapsed.as_secs_f64()) as u64;

    println!("\n[Single-Thread] check_model (RPM + TPM):");
    println!("  Iterations:      {}", MODEL_ITERS);
    println!("  Total Time:      {:?}", elapsed);
    println!("  Latency / Op:    {:.2} ns ({:.4} µs)", nanos_per_op, nanos_per_op / 1000.0);
    println!("  Throughput:      {} ops/sec", ops_per_sec);
    assert!(
        nanos_per_op < max_allowed,
        "check_model overhead must be within threshold (<{:.0}ns), got {:.2}ns",
        max_allowed,
        nanos_per_op
    );

    // 3. Multi-Threaded Concurrent Throughput Benchmark
    const THREADS: usize = 8;
    const OPS_PER_THREAD: usize = 100_000;
    let total_concurrent_ops = THREADS * OPS_PER_THREAD;

    let start = Instant::now();
    let mut handles = Vec::with_capacity(THREADS);
    for t in 0..THREADS {
        let lim = Arc::clone(&limiter);
        let ips_clone = ips.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..OPS_PER_THREAD {
                let ip = &ips_clone[(t * 32 + (i % 32)) % ips_clone.len()];
                let model = if i % 2 == 0 { "gpt-4o" } else { "claude-3-7-sonnet" };
                let _ = lim.check_ip(ip);
                let _ = lim.check_model(model, 10);
            }
        }));
    }

    for h in handles {
        h.join().expect("thread finished");
    }
    let elapsed = start.elapsed();
    // Each iteration did 1 check_ip and 1 check_model = 2 operations
    let total_operations = total_concurrent_ops * 2;
    let concurrent_nanos = elapsed.as_nanos() as f64 / total_operations as f64;
    let concurrent_ops_per_sec = (total_operations as f64 / elapsed.as_secs_f64()) as u64;

    println!("\n[Multi-Thread Concurrency] ({} threads, {} total ops):", THREADS, total_operations);
    println!("  Total Time:      {:?}", elapsed);
    println!("  Latency / Op:    {:.2} ns ({:.4} µs)", concurrent_nanos, concurrent_nanos / 1000.0);
    println!("  Throughput:      {} ops/sec", concurrent_ops_per_sec);
    println!("========================================================\n");

    assert!(
        concurrent_nanos < max_allowed,
        "Concurrent rate limiter must maintain overhead within threshold (<{:.0}ns), got {:.2}ns",
        max_allowed,
        concurrent_nanos
    );
}
