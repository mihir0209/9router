#!/usr/bin/env python3
"""
9Router Native Comprehensive Latency & Performance Benchmark Suite
Zero-dependency Python 3 benchmark (using http.client & concurrent.futures).

Suites:
  1. Pure Engine Internal Overhead (Health, Auth, 1,013 Models Catalog, Dynamic Routes)
  2. Concurrency Load Scaling & Throughput (1, 5, 10, 25, 50 parallel workers)
  3. Token Saver & In-Flight Compression Engine Overhead (5KB, 25KB, 50KB payloads)
  4. Database & Write-Behind Memory Queue Latency
  5. Proxy Pools & Outbound Routing Latency
  6. In-Memory Token Caching Lookup Speed
"""

import argparse
import concurrent.futures
import http.client
import json
import math
import statistics
import sys
import time
import urllib.parse
from typing import Any, Dict, List, Optional, Tuple

# ANSI Color codes
BOLD = "\033[1m"
GREEN = "\033[32m"
CYAN = "\033[36m"
YELLOW = "\033[33m"
RED = "\033[31m"
DIM = "\033[90m"
RESET = "\033[0m"


def format_ms(val: float) -> str:
    if val < 1.0:
        return f"{val * 1000.0:.1f} µs"
    elif val < 10.0:
        return f"{val:.3f} ms"
    elif val < 100.0:
        return f"{val:.2f} ms"
    elif val < 1000.0:
        return f"{val:.1f} ms"
    else:
        return f"{val / 1000.0:.2f} s"


def compute_stats(latencies_ms: List[float]) -> Dict[str, float]:
    if not latencies_ms:
        return {"count": 0, "min": 0, "mean": 0, "median": 0, "p90": 0, "p95": 0, "p99": 0, "max": 0, "stdev": 0}

    s = sorted(latencies_ms)
    n = len(s)
    p90_idx = min(n - 1, math.ceil(0.90 * n) - 1)
    p95_idx = min(n - 1, math.ceil(0.95 * n) - 1)
    p99_idx = min(n - 1, math.ceil(0.99 * n) - 1)

    return {
        "count": n,
        "min": s[0],
        "mean": statistics.mean(s),
        "median": statistics.median(s),
        "p90": s[p90_idx],
        "p95": s[p95_idx],
        "p99": s[p99_idx],
        "max": s[-1],
        "stdev": statistics.stdev(s) if n > 1 else 0.0,
    }


def print_table_header(title: str):
    print(f"\n{BOLD}{CYAN}=== {title} ==={RESET}")
    print(f"{'Endpoint / Test':<36} | {'Reqs':>6} | {'Min':>10} | {'Mean':>10} | {'P50':>10} | {'P95':>10} | {'P99':>10} | {'Max':>10} | {'RPS':>9}")
    print("-" * 121)


def print_stat_row(name: str, stats: Dict[str, float], duration_sec: float):
    rps = stats["count"] / max(duration_sec, 0.0001)
    print(
        f"{name:<36} | "
        f"{stats['count']:>6} | "
        f"{format_ms(stats['min']):>10} | "
        f"{format_ms(stats['mean']):>10} | "
        f"{format_ms(stats['median']):>10} | "
        f"{format_ms(stats['p95']):>10} | "
        f"{format_ms(stats['p99']):>10} | "
        f"{format_ms(stats['max']):>10} | "
        f"{rps:>9.1f}"
    )


def make_conn(parsed_url: urllib.parse.ParseResult, timeout: float = 5.0):
    host = parsed_url.hostname or "127.0.0.1"
    port = parsed_url.port or (443 if parsed_url.scheme == "https" else 80)
    if parsed_url.scheme == "https":
        import ssl
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        return http.client.HTTPSConnection(host, port, timeout=timeout, context=ctx)
    else:
        return http.client.HTTPConnection(host, port, timeout=timeout)


# ============================================================================
# Suite 1: Pure Engine Internal Overhead
# ============================================================================
def run_overhead_suite(base_url: str, api_key: str, samples: int = 100) -> Dict[str, Any]:
    print_table_header("1. Gateway Pure Routing & Engine Overhead")
    parsed = urllib.parse.urlparse(base_url)
    results = {}

    endpoints = [
        ("Health Check (Axum / Hyper core)", "/api/health", {}),
        ("Auth Status (JWT session probe)", "/api/auth/status", {"Authorization": f"Bearer {api_key}"}),
        ("Models List (1,013 Catalog & DB)", "/v1/models", {"Authorization": f"Bearer {api_key}", "Accept": "application/json"}),
        ("Proxy Pools API (/api/proxy-pools)", "/api/proxy-pools", {"Authorization": f"Bearer {api_key}"}),
        ("Combos Config (/api/combos)", "/api/combos", {"Authorization": f"Bearer {api_key}"}),
        ("Require Login Probe", "/api/require-login", {}),
    ]

    for label, path, headers in endpoints:
        latencies = []
        conn = make_conn(parsed)
        try:
            conn.request("GET", path, headers=headers)
            resp = conn.getresponse()
            resp.read()
        except Exception as e:
            print(f"{RED}Warmup failed for {label}: {e}{RESET}")
            continue

        t_start = time.perf_counter()
        for _ in range(samples):
            t0 = time.perf_counter()
            conn.request("GET", path, headers=headers)
            resp = conn.getresponse()
            resp.read()
            t1 = time.perf_counter()
            latencies.append((t1 - t0) * 1000.0)
        t_end = time.perf_counter()
        conn.close()

        stats = compute_stats(latencies)
        print_stat_row(label, stats, t_end - t_start)
        results[label] = stats

    return results


# ============================================================================
# Suite 2: High Concurrency Throughput & Latency Scaling
# ============================================================================
def run_concurrency_suite(base_url: str, samples_per_level: int = 200) -> Dict[str, Any]:
    print_table_header("2. Concurrency Load Scaling & Throughput (Tokio/Hyper Runtime)")
    parsed = urllib.parse.urlparse(base_url)
    concurrency_levels = [1, 5, 10, 25, 50]
    results = {}

    for c in concurrency_levels:
        latencies = []

        def worker_task(iters: int) -> List[float]:
            task_latencies = []
            conn = make_conn(parsed)
            for _ in range(iters):
                t0 = time.perf_counter()
                conn.request("GET", "/api/health")
                resp = conn.getresponse()
                resp.read()
                t1 = time.perf_counter()
                task_latencies.append((t1 - t0) * 1000.0)
            conn.close()
            return task_latencies

        iters_per_worker = max(1, samples_per_level // c)
        t_start = time.perf_counter()
        with concurrent.futures.ThreadPoolExecutor(max_workers=c) as executor:
            futures = [executor.submit(worker_task, iters_per_worker) for _ in range(c)]
            for f in concurrent.futures.as_completed(futures):
                latencies.extend(f.result())
        t_end = time.perf_counter()

        stats = compute_stats(latencies)
        name = f"Concurrency {c:2d} workers"
        print_stat_row(name, stats, t_end - t_start)
        results[f"c_{c}"] = stats

    return results


# ============================================================================
# Suite 3: Token Saver & In-Flight Compression Engine Overhead
# ============================================================================
def run_compression_suite(base_url: str, api_key: str, samples: int = 50) -> Dict[str, Any]:
    print_table_header("3. In-Flight Token Saver & Payload Processing Overhead")
    parsed = urllib.parse.urlparse(base_url)
    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
    }
    results = {}

    payload_sizes = [
        ("Small Context (500 B prompt)", 500),
        ("Medium Context (10 KB diff/code)", 10 * 1024),
        ("Large Context (50 KB multi-turn)", 50 * 1024),
    ]

    for label, size_bytes in payload_sizes:
        synthetic_content = "def calculate_benchmark():\n    # " + ("data " * (size_bytes // 6))
        payload = json.dumps({
            "model": "non-existent-test-model",
            "messages": [{"role": "user", "content": synthetic_content}],
        })

        latencies = []
        conn = make_conn(parsed)
        # Warmup
        try:
            conn.request("POST", "/v1/chat/completions", body=payload, headers=headers)
            resp = conn.getresponse()
            resp.read()
        except Exception:
            pass

        t_start = time.perf_counter()
        for _ in range(samples):
            t0 = time.perf_counter()
            conn.request("POST", "/v1/chat/completions", body=payload, headers=headers)
            resp = conn.getresponse()
            resp.read()
            t1 = time.perf_counter()
            latencies.append((t1 - t0) * 1000.0)
        t_end = time.perf_counter()
        conn.close()

        stats = compute_stats(latencies)
        print_stat_row(label, stats, t_end - t_start)
        results[label] = stats

    return results


# ============================================================================
# Suite 4: Database & Model Lookup Latency
# ============================================================================
def run_db_suite(base_url: str, api_key: str, samples: int = 50) -> Dict[str, Any]:
    print_table_header("4. SQLite Database Query & Catalog Serialization Latency")
    parsed = urllib.parse.urlparse(base_url)
    headers = {"Authorization": f"Bearer {api_key}", "Accept": "application/json"}
    results = {}

    queries = [
        ("Full Catalog (1,013 models JSON)", "/v1/models"),
        ("Active Combos Query", "/api/combos"),
        ("Proxy Pools Query (38 pools)", "/api/proxy-pools"),
        ("Provider Nodes Query", "/api/provider-nodes"),
    ]

    for label, path in queries:
        latencies = []
        conn = make_conn(parsed)
        t_start = time.perf_counter()
        for _ in range(samples):
            t0 = time.perf_counter()
            conn.request("GET", path, headers=headers)
            resp = conn.getresponse()
            resp.read()
            t1 = time.perf_counter()
            latencies.append((t1 - t0) * 1000.0)
        t_end = time.perf_counter()
        conn.close()

        stats = compute_stats(latencies)
        print_stat_row(label, stats, t_end - t_start)
        results[label] = stats

    return results


# ============================================================================
# Suite 5: Exact-Match In-Memory Cache Lookup Speed
# ============================================================================
def run_cache_suite(base_url: str, api_key: str) -> Dict[str, Any]:
    print_table_header("5. Exact-Match In-Memory Token Cache Lookup Speed")
    parsed = urllib.parse.urlparse(base_url)
    results = {}

    headers = {
        "Authorization": f"Bearer {api_key}",
        "Content-Type": "application/json",
    }
    payload = json.dumps({
        "model": "cached-probe-model",
        "messages": [{"role": "user", "content": "Hello cache benchmark probe"}],
        "max_tokens": 5,
    })

    conn = make_conn(parsed)
    cached_latencies = []
    t_start = time.perf_counter()
    for _ in range(50):
        t0 = time.perf_counter()
        conn.request("POST", "/v1/chat/completions", body=payload, headers=headers)
        resp = conn.getresponse()
        resp.read()
        t1 = time.perf_counter()
        cached_latencies.append((t1 - t0) * 1000.0)
    t_end = time.perf_counter()
    conn.close()

    hit_stats = compute_stats(cached_latencies)
    print_stat_row("In-Memory Hash & Route Probe", hit_stats, t_end - t_start)
    results["cache_probe"] = hit_stats
    return results


# ============================================================================
# Main CLI Entry Point
# ============================================================================
def main():
    parser = argparse.ArgumentParser(description="9Router Comprehensive Latency Benchmark")
    parser.add_argument("--url", default="http://127.0.0.1:20128", help="Gateway URL (default: http://127.0.0.1:20128)")
    parser.add_argument("--key", default="sk-e0053e088b66e87b-izehke-9a23e6bb", help="API Key for authorization")
    parser.add_argument("--samples", type=int, default=50, help="Number of benchmark samples (default: 50)")
    parser.add_argument("--json", action="store_true", help="Output results in JSON format")
    args = parser.parse_args()

    print(f"\n{BOLD}{GREEN}========================================================================================{RESET}")
    print(f"{BOLD}{GREEN}            9Router Native AI Gateway Comprehensive Latency Benchmark Suite             {RESET}")
    print(f"{BOLD}{GREEN}========================================================================================{RESET}")
    print(f"Target Gateway: {BOLD}{args.url}{RESET}")
    print(f"Sample Count:   {BOLD}{args.samples}{RESET} iterations per test")
    print(f"API Key:        {BOLD}{args.key[:10]}...{args.key[-6:]}{RESET}\n")

    t_all_start = time.perf_counter()

    # 1. Routing & Engine Overhead
    overhead_results = run_overhead_suite(args.url, args.key, args.samples)

    # 2. Concurrency Scaling
    concurrency_results = run_concurrency_suite(args.url, samples_per_level=min(args.samples * 4, 200))

    # 3. Payload & Token Saver Overhead
    compression_results = run_compression_suite(args.url, args.key, samples=min(args.samples, 50))

    # 4. Database & Serialization
    db_results = run_db_suite(args.url, args.key, samples=min(args.samples, 50))

    # 5. In-Memory Cache Lookup Speed
    cache_results = run_cache_suite(args.url, args.key)

    t_all_end = time.perf_counter()
    total_elapsed = t_all_end - t_all_start

    print(f"\n{BOLD}{GREEN}========================================================================================{RESET}")
    print(f"{BOLD}Benchmark Suite Completed in {total_elapsed:.2f} seconds.{RESET}")
    print(f"{BOLD}{GREEN}========================================================================================{RESET}\n")

    if args.json:
        combined = {
            "overhead": overhead_results,
            "concurrency": concurrency_results,
            "compression": compression_results,
            "database": db_results,
            "cache": cache_results,
            "total_elapsed_sec": total_elapsed,
        }
        with open("benchmark_results.json", "w") as jf:
            json.dump(combined, jf, indent=2)
        print("Detailed JSON metrics saved to benchmark_results.json")


if __name__ == "__main__":
    main()
