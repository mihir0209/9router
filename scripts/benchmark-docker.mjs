#!/usr/bin/env node

/**
 * 9Router Docker Distroless & Compose Live Benchmark Suite
 *
 * Verifies:
 *   1. Image size (<25 MB target) & non-root UID 65532 via `docker image inspect`
 *   2. Container cold-boot time to HTTP 200 OK on `/health`
 *   3. Idle memory usage (MiB) via `docker stats --no-stream`
 *   4. HTTP latency percentiles (min/avg/p50/p95/p99/max) on `/health`, `/api/health`, `/v1/models`
 *   5. Clean teardown (`docker compose --profile <profile> down -v`)
 *
 * Usage:
 *   node scripts/benchmark-docker.mjs [--profile sqlite|postgres] [--image 9router:distroless-test] [--requests 100] [--skip-build] [--keep] [--json]
 */

import { execFileSync, spawnSync } from "node:child_process";
import { performance } from "node:perf_hooks";

const args = process.argv.slice(2);
function getArg(name, fallback) {
  const idx = args.indexOf(name);
  if (idx !== -1 && idx + 1 < args.length) return args[idx + 1];
  return fallback;
}
const hasFlag = (name) => args.includes(name);

const profile = getArg("--profile", "sqlite");
const image = getArg("--image", "9router:distroless-test");
const requests = Number.parseInt(getArg("--requests", "100"), 10);
const skipBuild = hasFlag("--skip-build");
const keep = hasFlag("--keep");
const jsonOnly = hasFlag("--json");
const port = Number.parseInt(getArg("--port", "20127"), 10);
const replicaPort = Number.parseInt(getArg("--replica-port", "20128"), 10);
const apiKey = getArg("--api-key", process.env.NINEROUTER_API_KEY || "");

function run(cmd, cmdArgs, opts = {}) {
  const res = spawnSync(cmd, cmdArgs, {
    encoding: "utf8",
    stdio: opts.inherit ? "inherit" : "pipe",
    env: { ...process.env, ...opts.env },
  });
  if (res.status !== 0 && !opts.allowFailure) {
    const err = res.stderr || res.stdout || `exit code ${res.status}`;
    throw new Error(`Command failed: ${cmd} ${cmdArgs.join(" ")}\n${err}`);
  }
  return res;
}

function log(...msg) {
  if (!jsonOnly) console.log(...msg);
}

function parseMemToMiB(memStr) {
  // e.g. "12.4MiB / 31.1GiB" or "1234KiB / ..." or "0.02GiB / ..."
  const used = (memStr || "").split("/")[0].trim();
  const match = /^([\d.]+)\s*(B|KiB|kB|MiB|MB|GiB|GB)$/i.exec(used);
  if (!match) return null;
  const val = Number.parseFloat(match[1]);
  const unit = match[2].toLowerCase();
  if (unit === "b") return val / (1024 * 1024);
  if (unit === "kib") return val / 1024;
  if (unit === "kb") return (val * 1000) / (1024 * 1024);
  if (unit === "mib") return val;
  if (unit === "mb") return (val * 1_000_000) / (1024 * 1024);
  if (unit === "gib") return val * 1024;
  if (unit === "gb") return (val * 1_000_000_000) / (1024 * 1024);
  return null;
}

function computeStats(latenciesMs) {
  const sorted = [...latenciesMs].sort((a, b) => a - b);
  const n = sorted.length;
  const sum = sorted.reduce((a, b) => a + b, 0);
  const pick = (p) => sorted[Math.min(n - 1, Math.max(0, Math.ceil(n * p) - 1))];
  return {
    samples: n,
    min_ms: Number(sorted[0].toFixed(3)),
    avg_ms: Number((sum / n).toFixed(3)),
    p50_ms: Number(pick(0.5).toFixed(3)),
    p95_ms: Number(pick(0.95).toFixed(3)),
    p99_ms: Number(pick(0.99).toFixed(3)),
    max_ms: Number(sorted[n - 1].toFixed(3)),
  };
}

async function waitForHealth(baseUrl, timeoutMs = 30_000) {
  const start = performance.now();
  while (performance.now() - start < timeoutMs) {
    try {
      const resp = await fetch(`${baseUrl}/health`, { signal: AbortSignal.timeout(1000) });
      if (resp.status === 200) {
        const body = await resp.text();
        return { elapsedMs: performance.now() - start, status: resp.status, body };
      }
    } catch {
      // Wait for listener bind
    }
    await new Promise((r) => setTimeout(r, 25));
  }
  throw new Error(`Timed out waiting for ${baseUrl}/health after ${timeoutMs}ms`);
}

async function benchEndpoint(url, count, headers = {}) {
  // Warmup
  const warmup = await fetch(url, { headers });
  await warmup.arrayBuffer();

  const latencies = [];
  let statusCode = warmup.status;
  for (let i = 0; i < count; i++) {
    const t0 = performance.now();
    const res = await fetch(url, { headers });
    await res.arrayBuffer();
    const t1 = performance.now();
    statusCode = res.status;
    latencies.push(t1 - t0);
  }
  return {
    url,
    http_status: statusCode,
    ...computeStats(latencies),
  };
}

async function main() {
  if (!["sqlite", "postgres"].includes(profile)) {
    throw new Error(`Unsupported profile '${profile}'. Use 'sqlite' or 'postgres'.`);
  }

  log("================================================================================");
  log(`🐳 9Router Docker Distroless Verification & Benchmark (profile: ${profile})`);
  log("================================================================================");

  if (!skipBuild) {
    log(`\n[1/5] Building Docker image '${image}' from Dockerfile.distroless...`);
    run("docker", ["build", "-f", "Dockerfile.distroless", "-t", image, "."], { inherit: !jsonOnly });
  } else {
    log(`\n[1/5] Using existing Docker image '${image}' (--skip-build)...`);
  }

  // Inspect image metadata
  log("[2/5] Inspecting image size and security metadata...");
  const inspectRaw = execFileSync("docker", ["image", "inspect", image], { encoding: "utf8" });
  const inspectArr = JSON.parse(inspectRaw);
  const info = inspectArr[0];
  const sizeBytes = info.Size;
  const sizeMB = Number((sizeBytes / 1_000_000).toFixed(2));
  const sizeMiB = Number((sizeBytes / (1024 * 1024)).toFixed(2));
  const user = info.Config?.User || "";
  const arch = info.Architecture;
  const os = info.Os;

  if (sizeMB >= 25.0) {
    throw new Error(`Image size ${sizeMB} MB exceeds <25 MB target!`);
  }
  if (!user.startsWith("65532")) {
    throw new Error(`Expected non-root UID 65532, got '${user}'`);
  }

  log(`   Image:        ${image} (${os}/${arch})`);
  log(`   Size:         ${sizeBytes.toLocaleString()} bytes (${sizeMB} MB / ${sizeMiB} MiB) [<25 MB PASS]`);
  log(`   User:         ${user} [non-root 65532 PASS]`);

  const composeEnv = { NINEROUTER_IMAGE: image };
  const targetContainers =
    profile === "sqlite"
      ? ["9router-sqlite"]
      : ["9router-pg-primary", "9router-pg-replica", "9router-postgres-db"];

  // Ensure clean slate before measuring cold boot
  run("docker", ["compose", "--profile", profile, "down", "-v"], {
    allowFailure: true,
    env: composeEnv,
  });

  const results = {
    profile,
    image: {
      name: image,
      platform: `${os}/${arch}`,
      size_bytes: sizeBytes,
      size_mb: sizeMB,
      size_mib: sizeMiB,
      user,
    },
    cold_boot: {},
    stats: {},
    latency: {},
  };

  try {
    log(`\n[3/5] Starting 'docker compose --profile ${profile} up -d' and measuring cold boot...`);
    const bootT0 = performance.now();
    run("docker", ["compose", "--profile", profile, "up", "-d", "--no-build"], {
      env: composeEnv,
    });
    const composeUpMs = performance.now() - bootT0;

    const primaryHealth = await waitForHealth(`http://127.0.0.1:${port}`);
    const totalBootMs = performance.now() - bootT0;
    results.cold_boot.primary = {
      compose_up_ms: Number(composeUpMs.toFixed(2)),
      health_wait_ms: Number(primaryHealth.elapsedMs.toFixed(2)),
      total_to_200_ms: Number(totalBootMs.toFixed(2)),
      response: primaryHealth.body,
    };
    log(
      `   Primary /health 200 OK in ${totalBootMs.toFixed(1)} ms ` +
        `(compose up: ${composeUpMs.toFixed(1)} ms, ready wait: ${primaryHealth.elapsedMs.toFixed(1)} ms)`
    );

    if (profile === "postgres") {
      const replicaHealth = await waitForHealth(`http://127.0.0.1:${replicaPort}`);
      const replicaTotalMs = performance.now() - bootT0;
      results.cold_boot.replica = {
        health_wait_ms: Number(replicaHealth.elapsedMs.toFixed(2)),
        total_to_200_ms: Number(replicaTotalMs.toFixed(2)),
        response: replicaHealth.body,
      };
      log(`   Replica /health 200 OK in ${replicaTotalMs.toFixed(1)} ms`);
    }

    // Settle briefly before measuring idle memory
    await new Promise((r) => setTimeout(r, 1000));

    log("\n[4/5] Measuring idle container memory & CPU (docker stats --no-stream)...");
    const statsRaw = execFileSync(
      "docker",
      [
        "stats",
        "--no-stream",
        "--format",
        "{{.Name}}|{{.MemUsage}}|{{.MemPerc}}|{{.CPUPerc}}|{{.PIDs}}",
        ...targetContainers,
      ],
      { encoding: "utf8" }
    ).trim();

    for (const line of statsRaw.split("\n").filter(Boolean)) {
      const [name, memUsage, memPerc, cpuPerc, pids] = line.split("|");
      const memMiB = parseMemToMiB(memUsage);
      results.stats[name] = {
        mem_usage: memUsage,
        mem_mib: memMiB !== null ? Number(memMiB.toFixed(2)) : null,
        mem_perc: memPerc,
        cpu_perc: cpuPerc,
        pids: Number.parseInt(pids, 10),
      };
      log(`   ${name}: ${memUsage} (${memPerc}), CPU ${cpuPerc}, PIDs ${pids}`);
    }

    log(`\n[5/5] Running HTTP latency benchmarks (${requests} requests/endpoint)...`);
    const base = `http://127.0.0.1:${port}`;
    const modelHeaders = { Accept: "application/json" };
    if (apiKey) modelHeaders.Authorization = `Bearer ${apiKey}`;

    results.latency.health = await benchEndpoint(`${base}/health`, requests);
    results.latency.api_health = await benchEndpoint(`${base}/api/health`, requests);
    results.latency.v1_models = await benchEndpoint(`${base}/v1/models`, requests, modelHeaders);

    if (profile === "postgres") {
      const replicaBase = `http://127.0.0.1:${replicaPort}`;
      results.latency.replica_health = await benchEndpoint(`${replicaBase}/health`, requests);
      results.latency.replica_v1_models = await benchEndpoint(`${replicaBase}/v1/models`, requests, modelHeaders);
    }

    for (const [key, st] of Object.entries(results.latency)) {
      log(
        `   ${key} [HTTP ${st.http_status}]: min=${st.min_ms}ms avg=${st.avg_ms}ms ` +
          `p50=${st.p50_ms}ms p95=${st.p95_ms}ms p99=${st.p99_ms}ms max=${st.max_ms}ms`
      );
    }
  } finally {
    if (!keep) {
      log(`\nTearing down 'docker compose --profile ${profile} down -v'...`);
      run("docker", ["compose", "--profile", profile, "down", "-v"], {
        allowFailure: true,
        env: composeEnv,
      });
    } else {
      log("\nLeaving containers running (--keep).");
    }
  }

  if (jsonOnly) {
    console.log(JSON.stringify(results, null, 2));
  } else {
    log("\n✅ Docker distroless verification & benchmark complete.");
  }
}

main().catch((err) => {
  console.error(`❌ Benchmark failed: ${err.message}`);
  process.exit(1);
});
