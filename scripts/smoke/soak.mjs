#!/usr/bin/env node
// Bounded, low-rate soak against a deployed 9router Worker. NOT a stress test: one request every
// SOAK_INTERVAL_MS (default 2000, never below 500), a mixed read-only workload, one throwaway key.
//
//   SMOKE_BASE_URL=https://x.workers.dev SMOKE_ENV_FILE=<env file with ADMIN_API_TOKEN> \
//   [SMOKE_VERSION_OVERRIDE=<version id>] [SOAK_SECONDS=720] [SOAK_INTERVAL_MS=2000] [SOAK_BURST=20] \
//   [SOAK_REPORT=out.json] node scripts/smoke/soak.mjs
//
// Phases: (1) cold burst: SOAK_BURST concurrent GET /v1/models (a request that hangs is recorded as status 0),
// (2) the timed mix: health, /v1/models, /v1/models If-None-Match, chat with an invalid key, /login, a static
// asset, (3) a second burst. Every sample (label, ms, status, kind) is kept for exact nearest-rank percentiles.
// kind "cpu-kill" = a non-JSON 503/500 produced by Cloudflare itself (error 1102 or 1101), not by the app.
// Secrets are never printed. The throwaway key is deleted at the end (one auditLog row per create and delete).
import fs from "node:fs";

function loadEnvFile(file) {
  const out = {};
  if (!file || !fs.existsSync(file)) return out;
  for (const line of fs.readFileSync(file, "utf8").split("\n")) {
    const m = /^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)=(.*)$/.exec(line);
    if (m) out[m[1]] = m[2].trim().replace(/^["']|["']$/g, "");
  }
  return out;
}
const fileEnv = loadEnvFile(process.env.SMOKE_ENV_FILE);
const pick = (...n) => { for (const k of n) if (process.env[k]) return process.env[k]; for (const k of n) if (fileEnv[k]) return fileEnv[k]; return ""; };

const BASE = pick("SMOKE_BASE_URL").replace(/\/+$/, "");
const ADMIN = pick("SMOKE_ADMIN_TOKEN", "ADMIN_API_TOKEN");
const OVERRIDE = pick("SMOKE_VERSION_OVERRIDE");
const WORKER = new URL(BASE || "http://x").hostname.split(".")[0];
const SECONDS = Number(pick("SOAK_SECONDS") || 720);
const INTERVAL = Math.max(500, Number(pick("SOAK_INTERVAL_MS") || 2000));
const BURST = Number(pick("SOAK_BURST") || 20);
const TIMEOUT = Number(pick("SOAK_TIMEOUT_MS") || 45000);
const REPORT = pick("SOAK_REPORT");
if (!/^https?:\/\//.test(BASE) || !ADMIN) { console.error("SMOKE_BASE_URL and an admin token are required"); process.exit(2); }

const baseHeaders = OVERRIDE ? { "cloudflare-workers-version-overrides": `${WORKER}="${OVERRIDE}"` } : {};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const samples = [];

async function req(label, path, init = {}) {
  const t0 = performance.now();
  let status = 0, kind = "ok", json = false, etag = "";
  try {
    const res = await fetch(BASE + path, { redirect: "manual", signal: AbortSignal.timeout(TIMEOUT), ...init, headers: { ...baseHeaders, ...(init.headers || {}) } });
    await res.arrayBuffer();
    status = res.status;
    etag = res.headers.get("etag") || "";
    json = (res.headers.get("content-type") || "").includes("json");
    if (status >= 500) kind = json ? "app-5xx" : "cpu-kill";
  } catch (e) {
    kind = e.name === "TimeoutError" ? "hung" : "network";
  }
  const ms = performance.now() - t0;
  samples.push({ label, ms: Math.round(ms * 10) / 10, status, kind, t: Date.now() });
  return { status, etag, kind };
}

const admin = { authorization: `Bearer ${ADMIN}`, "content-type": "application/json" };
async function adminCall(method, path, body) {
  for (let i = 0; i < 80; i++) { // admin writes are CPU heavy: on a CPU-limited plan most attempts are killed
    if (i > 0) await fetch(BASE + "/api/health", { headers: baseHeaders, signal: AbortSignal.timeout(TIMEOUT) }).then((r) => r.arrayBuffer()).catch(() => {}); // warm an isolate first
    try {
      const res = await fetch(BASE + path, { method, headers: { ...baseHeaders, ...admin }, body: body && JSON.stringify(body), signal: AbortSignal.timeout(TIMEOUT) });
      if (res.status < 500) return { status: res.status, json: await res.json().catch(() => null) };
    } catch { /* retry */ }
    await sleep(1500);
  }
  return { status: 0, json: null };
}

async function burst(name) {
  const key = state.key;
  const t0 = performance.now();
  const rs = await Promise.all(Array.from({ length: BURST }, () => req(`${name}`, "/v1/models", { headers: { authorization: `Bearer ${key}` } })));
  const bad = rs.filter((r) => r.status !== 200).length;
  console.log(`${name}: ${BURST} concurrent GET /v1/models in ${Math.round(performance.now() - t0)} ms, non-200: ${bad} (${rs.filter((r) => r.kind === "hung").length} hung)`);
}

const state = { key: "", id: "" };
const pct = (sorted, p) => (sorted.length ? sorted[Math.min(sorted.length, Math.max(1, Math.ceil((p / 100) * sorted.length))) - 1] : null);
function summarize(list) {
  const ms = list.map((s) => s.ms).sort((a, b) => a - b);
  const done = list.filter((s) => s.status > 0 && s.status < 500).map((s) => s.ms).sort((a, b) => a - b);
  const count = (k) => list.filter((s) => s.kind === k).length;
  return { n: list.length, p50: pct(ms, 50), p95: pct(ms, 95), p99: pct(ms, 99), max: ms[ms.length - 1], completedP50: pct(done, 50), completedP95: pct(done, 95), completedP99: pct(done, 99), cpuKill: count("cpu-kill"), app5xx: count("app-5xx"), hung: count("hung"), network: count("network") };
}

console.log(`soak target ${BASE}${OVERRIDE ? " version " + OVERRIDE.slice(0, 8) : " (live)"}: ${SECONDS}s, 1 request / ${INTERVAL} ms, burst ${BURST}`);
const started = new Date().toISOString();
try {
  const created = await adminCall("POST", "/api/admin/keys", { ownerRef: "smoke-test", name: `smoke-test-DELETE-ME-soak-${Date.now()}`, rateLimitRequests: 100000, rateLimitWindowSeconds: 3600 });
  if (created.status !== 201) throw new Error(`could not create the throwaway key (HTTP ${created.status})`);
  state.key = created.json.key; state.id = created.json.id;
  // wait until the key is visible to the serving isolate(s)
  for (let i = 0; i < 20; i++) {
    const r = await fetch(BASE + "/v1/models", { headers: { ...baseHeaders, authorization: `Bearer ${state.key}` }, signal: AbortSignal.timeout(TIMEOUT) }).catch(() => null);
    if (r && r.status === 200) break;
    await sleep(3000);
  }
  samples.length = 0;
  await burst("burst-start");
  const end = Date.now() + SECONDS * 1000;
  let etag = "", i = 0;
  const loginHtml = await fetch(BASE + "/login", { headers: baseHeaders, signal: AbortSignal.timeout(TIMEOUT) }).then((r) => r.text()).catch(() => "");
  const assets = [/\/_next\/static\/[A-Za-z0-9_\-./%]+\.js/.exec(loginHtml)?.[0] || "/_next/static/missing.js"];
  while (Date.now() < end) {
    const turn = i++ % 6;
    if (turn === 0) await req("health", "/api/health");
    else if (turn === 1) etag = (await req("models", "/v1/models", { headers: { authorization: `Bearer ${state.key}` } })).etag || etag;
    else if (turn === 2) await req("models-inm", "/v1/models", { headers: { authorization: `Bearer ${state.key}`, ...(etag ? { "if-none-match": etag } : {}) } });
    else if (turn === 3) await req("chat-badkey", "/v1/chat/completions", { method: "POST", headers: { authorization: "Bearer sk-invalid-soak-key-000000", "content-type": "application/json" }, body: JSON.stringify({ model: "x/y", messages: [{ role: "user", content: "hi" }] }) });
    else if (turn === 4) await req("login-page", "/login");
    else await req("static-asset", assets[0]);
    await sleep(INTERVAL);
  }
  await burst("burst-end");
} catch (e) {
  console.error("soak aborted:", e.message);
} finally {
  if (state.id) {
    const d = await adminCall("DELETE", `/api/admin/keys/${encodeURIComponent(state.id)}?ownerRef=smoke-test`);
    console.log(`throwaway key delete -> HTTP ${d.status}`);
  }
}
const ended = new Date().toISOString();
const byLabel = {};
for (const s of samples) (byLabel[s.label] ||= []).push(s);
const result = { base: BASE, version: OVERRIDE || "live", started, ended, total: summarize(samples), byLabel: Object.fromEntries(Object.entries(byLabel).map(([k, v]) => [k, summarize(v)])) };
console.log(`window ${started} .. ${ended}`);
console.log(`total: ${JSON.stringify(result.total)}`);
for (const [k, v] of Object.entries(result.byLabel)) console.log(`  ${k.padEnd(18)} n=${String(v.n).padStart(4)} p50=${v.p50} p95=${v.p95} p99=${v.p99} max=${v.max}  completed p50/p95/p99=${v.completedP50}/${v.completedP95}/${v.completedP99}  cpu-kill=${v.cpuKill} app5xx=${v.app5xx} hung=${v.hung}`);
if (REPORT) fs.writeFileSync(REPORT, JSON.stringify({ ...result, samples }, null, 1));
