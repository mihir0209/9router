#!/usr/bin/env node
// Functional smoke / regression run against a DEPLOYED 9router instance (for example the
// Cloudflare Worker). Plain Node ESM, no dependencies, low traffic by design.
//
//   SMOKE_BASE_URL=https://example.workers.dev \
//   SMOKE_ADMIN_TOKEN=... \            # ADMIN_API_TOKEN of the target (creates/deletes throwaway keys)
//   [SMOKE_DASHBOARD_PASSWORD=...] \   # enables the login / dashboard page cases
//   [SMOKE_API_KEY=...] \              # use an existing key instead of creating throwaway ones
//   node scripts/smoke/deployed-smoke.mjs
//
// Credentials come from the environment or from a git-ignored env file (default
// scripts/smoke/.env.smoke, override with SMOKE_ENV_FILE; KEY=value lines; ADMIN_API_TOKEN and
// INITIAL_PASSWORD are accepted as aliases so a repo .env can be pointed at directly).
// Secrets are never printed: every output line is passed through a redactor.
//
// Optional knobs:
//   SMOKE_CHAT_MODEL        cheap model for the few real provider calls (default cf/@cf/meta/llama-3.2-1b-instruct)
//   SMOKE_CHAT_PROVIDER     real provider id of the chat model when its prefix is an alias (for example cf -> cloudflare-ai);
//                           forceProvider must be the provider id, not the alias (default: the model prefix)
//   SMOKE_OTHER_PROVIDER    a provider id different from the chat model's, for the pinned-provider 403 case
//   SMOKE_OTHER_MODEL       a model id that is not the chat model, for the ACL / pin cases (default openai/gpt-4o)
//   SMOKE_REQUEST_TIMEOUT_MS per-request timeout; a request that hangs is recorded as status 0 "timeout" (default 45000)
//   SMOKE_KEY_WAIT_MS       how long to wait for a freshly created key to be visible to other isolates (default 60000)
//   SMOKE_MAX_REAL_CALLS    hard cap on upstream provider calls for the whole run (default 8)
//   SMOKE_SOAK_SECONDS      mixed low-rate traffic phase, 0 to skip (default 0)
//   SMOKE_SOAK_INTERVAL_MS  gap between soak requests (default 1500; never below 1000)
//   SMOKE_OVERSIZE=1        also upload a body above the JSON cap and expect 413 (slow on a slow uplink)
//   SMOKE_CRON_SECRET       send it to /api/cron/flush; without it the case only records whether it is open
//   SMOKE_VERSION_OVERRIDE  Worker version id to pin every request to (Cloudflare-Workers-Version-Overrides header),
//                           for testing a 0% gradual-deployment version while the live version keeps 100% of traffic
//   SMOKE_WORKER_NAME       script name for that header (default: first label of the host name)
//   SMOKE_REPORT            write the JSON result (cases + latency samples) to this path
//   SMOKE_KEEP_KEYS=1       do not delete the throwaway keys (debugging only)
//   SMOKE_JWT_SECRET        the Worker's JWT_SECRET (JWT_SECRET in the env file is accepted too). Used ONLY when the password
//                           login is missing or rejected: the script then signs its own short-lived `auth_token` session cookie
//                           (HS256, the same claims the app issues), in memory, for this run. Never printed, never written.
//                           Use it only against your own deployment.
//   SMOKE_DASHBOARD_PASSWORD_2 / _3  further candidate passwords, tried (once each, 30 s apart) after the first is rejected
//
// Audit noise: every admin-API key create and delete writes one auditLog row (a full run creates about 7 keys, so about 14
// rows) and the dashboard key CRUD case adds a few. There is no supported API to prune auditLog.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

function loadEnvFile(file) {
  const out = {};
  if (!file || !fs.existsSync(file)) return out;
  for (const line of fs.readFileSync(file, "utf8").split("\n")) {
    const m = /^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)=(.*)$/.exec(line);
    if (m) out[m[1]] = m[2].trim().replace(/^["']|["']$/g, "");
  }
  return out;
}

const fileEnv = loadEnvFile(process.env.SMOKE_ENV_FILE || path.join(here, ".env.smoke"));
const pick = (...names) => {
  for (const n of names) if (process.env[n]) return process.env[n];
  for (const n of names) if (fileEnv[n]) return fileEnv[n];
  return "";
};

const BASE = pick("SMOKE_BASE_URL").replace(/\/+$/, "");
const ADMIN_TOKEN = pick("SMOKE_ADMIN_TOKEN", "ADMIN_API_TOKEN");
const GIVEN_KEY = pick("SMOKE_API_KEY");
const CRON_SECRET = pick("SMOKE_CRON_SECRET");
const CHAT_MODEL = pick("SMOKE_CHAT_MODEL") || "cf/@cf/meta/llama-3.2-1b-instruct";
const OTHER_PROVIDER = pick("SMOKE_OTHER_PROVIDER") || (CHAT_MODEL.startsWith("ollama/") ? "openai" : "ollama");
const OTHER_MODEL = pick("SMOKE_OTHER_MODEL") || "openai/gpt-4o"; // only ever asked for, never reaches a provider (denied or rewritten first)
const MODEL_LEAF = CHAT_MODEL.slice(CHAT_MODEL.indexOf("/") + 1);
const CHAT_PROVIDER = pick("SMOKE_CHAT_PROVIDER") || CHAT_MODEL.slice(0, CHAT_MODEL.indexOf("/"));
const REQUEST_TIMEOUT_MS = Number(pick("SMOKE_REQUEST_TIMEOUT_MS") || 45000);
const KEY_WAIT_MS = Number(pick("SMOKE_KEY_WAIT_MS") || 60000);
const MAX_REAL = Number(pick("SMOKE_MAX_REAL_CALLS") || 8);
const SOAK_SECONDS = Number(pick("SMOKE_SOAK_SECONDS") || 0);
const SOAK_INTERVAL = Math.max(1000, Number(pick("SMOKE_SOAK_INTERVAL_MS") || 1500));
const OVERSIZE = pick("SMOKE_OVERSIZE") === "1";
const KEEP_KEYS = pick("SMOKE_KEEP_KEYS") === "1";
const REPORT = pick("SMOKE_REPORT");
const VERSION_OVERRIDE = pick("SMOKE_VERSION_OVERRIDE");
const PASSWORDS = [pick("SMOKE_DASHBOARD_PASSWORD"), pick("SMOKE_DASHBOARD_PASSWORD_2"), pick("SMOKE_DASHBOARD_PASSWORD_3")].filter(Boolean);
const JWT_SECRET = process.env.SMOKE_JWT_SECRET || fileEnv.SMOKE_JWT_SECRET || fileEnv.JWT_SECRET || "";
const WORKER_NAME = pick("SMOKE_WORKER_NAME") || new URL(BASE || "http://x").hostname.split(".")[0];
const baseHeaders = VERSION_OVERRIDE ? { "cloudflare-workers-version-overrides": `${WORKER_NAME}="${VERSION_OVERRIDE}"` } : {};
const OWNER = "smoke-test";
const STAMP = new Date().toISOString().replace(/[-:.TZ]/g, "").slice(0, 14);

if (!/^https?:\/\//.test(BASE)) {
  console.error("SMOKE_BASE_URL is required (for example https://your-worker.example.workers.dev)");
  process.exit(2);
}

// ---------- output helpers ----------
const secrets = new Set([ADMIN_TOKEN, ...PASSWORDS, JWT_SECRET, GIVEN_KEY, CRON_SECRET].filter((s) => s && s.length >= 6));
const createdKeySecrets = new Set();
function redact(text) {
  let out = String(text);
  for (const s of [...secrets, ...createdKeySecrets, ...sessionSecrets]) out = out.split(s).join("<redacted>");
  return out.replace(/eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}/g, "<jwt-redacted>").replace(/sk-[A-Za-z0-9_-]{12,}/g, "sk-<redacted>").replace(/Bearer\s+[A-Za-z0-9._-]{16,}/g, "Bearer <redacted>");
}
const sessionSecrets = new Set();
const log = (...a) => console.log(redact(a.join(" ")));

const cases = [];
function record(id, name, status, detail = "") {
  cases.push({ id, name, status, detail: redact(detail) });
  const tag = { pass: "PASS", fail: "FAIL", skip: "SKIP", warn: "WARN" }[status];
  log(`${tag.padEnd(4)} ${id.padEnd(5)} ${name}${detail ? "  — " + detail : ""}`);
}
async function check(id, name, fn) {
  try {
    const r = await fn();
    if (r && r.skip) record(id, name, "skip", r.skip);
    else if (r && r.warn) record(id, name, "warn", r.warn);
    else record(id, name, "pass", (r && r.detail) || "");
  } catch (e) {
    record(id, name, "fail", e && e.message ? e.message : String(e));
  }
}
function expect(cond, message) {
  if (!cond) throw new Error(message);
}

// ---------- http + latency ----------
const samples = []; // { label, ms, status }
let realCalls = 0;
async function http(label, urlPath, init = {}) {
  const started = performance.now();
  let res, text;
  try {
    res = await fetch(BASE + urlPath, { redirect: "manual", signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS), ...init, headers: { ...baseHeaders, ...(init.headers || {}) } });
    text = init.stream ? null : await res.text();
  } catch (e) {
    const ms = performance.now() - started;
    samples.push({ label, ms, status: 0 });
    throw new Error(`${label}: ${e.name === "TimeoutError" ? `no response within ${REQUEST_TIMEOUT_MS} ms (hung request)` : e.message}`);
  }
  const ms = performance.now() - started;
  samples.push({ label, ms, status: res.status });
  return { res, text, ms, json() { try { return JSON.parse(text); } catch { return null; } } };
}
const bearer = (k) => ({ authorization: `Bearer ${k}` });
const jsonHeaders = (extra = {}) => ({ "content-type": "application/json", ...extra });
const adminHeaders = () => jsonHeaders(bearer(ADMIN_TOKEN));
const chatBody = (model, extra = {}) => JSON.stringify({ model, max_tokens: 16, messages: [{ role: "user", content: "Reply with the single word: ok" }], ...extra });

function percentile(sorted, p) {
  if (!sorted.length) return null;
  const rank = Math.ceil((p / 100) * sorted.length); // nearest-rank
  return sorted[Math.min(sorted.length, Math.max(1, rank)) - 1];
}
function summarize(list) {
  const ms = list.map((s) => s.ms).sort((a, b) => a - b);
  return { n: ms.length, min: ms[0], p50: percentile(ms, 50), p95: percentile(ms, 95), p99: percentile(ms, 99), max: ms[ms.length - 1] };
}

// ---------- admin key helpers ----------
const createdKeys = []; // { id, key }
async function createKey(suffix, policy = {}) {
  const body = { ownerRef: OWNER, name: `smoke-test-DELETE-ME-${STAMP}-${suffix}`, ...policy };
  let r;
  // A 5xx here on a CPU-limited plan (Workers Free: 503 "exceeded CPU") is retried on a fresh invocation. An attempt that
  // did complete leaves an extra throwaway key behind, which the cleanup phase removes (it deletes every smoke-test key).
  for (let attempt = 0; attempt < 5; attempt++) {
    r = await http("admin:create-key", "/api/admin/keys", { method: "POST", headers: adminHeaders(), body: JSON.stringify(body) });
    if (r.res.status < 500) break;
    await sleep(2000 * (attempt + 1));
  }
  if (r.res.status !== 201) return { status: r.res.status, body: r.json(), key: null };
  const j = r.json();
  createdKeySecrets.add(j.key);
  createdKeys.push({ id: j.id, key: j.key });
  return { status: 201, body: j, key: j.key, id: j.id };
}
async function deleteKey(id) {
  return http("admin:delete-key", `/api/admin/keys/${encodeURIComponent(id)}?ownerRef=${OWNER}`, { method: "DELETE", headers: adminHeaders() });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// A key created through one isolate becomes visible to the others after the control-version poll
// (about 10 s) or the unknown-key refresh window (30 s) described in docs/CLOUDFLARE_EXTREME_PROFILE.md.
// Rejections with 401 before that are expected, not counted by rate limits, and do not reach a provider.
const visibility = [];
let cpuKills = 0; // retried invocations that Cloudflare killed (503/1102 non-JSON), see whenVisible
async function whenVisible(label, key, send) {
  const started = Date.now();
  let r = await send();
  // 401 = the serving isolate has not seen the new key yet. 503 / 1102 from Cloudflare (not from the app) = the invocation was
  // killed by a CPU limit; the policy under test was not evaluated, so it is retried on a fresh invocation and counted.
  const killed = (x) => x.res.status === 503 && !(x.res.headers.get("content-type") || "").includes("json");
  while ((r.res.status === 401 || killed(r)) && Date.now() - started < KEY_WAIT_MS) {
    if (killed(r)) cpuKills++;
    await sleep(r.res.status === 401 ? 4000 : 1500);
    r = await send();
  }
  if (Date.now() - started > 1500) visibility.push({ label, seconds: Math.round((Date.now() - started) / 1000) });
  return r;
}

// ---------- cases ----------
async function publicCases() {
  await check("P1", "GET /api/health is 200 {ok:true}", async () => {
    const r = await http("health", "/api/health");
    expect(r.res.status === 200, `status ${r.res.status}`);
    expect(r.json()?.ok === true, `body ${r.text.slice(0, 80)}`);
  });
  await check("P2", "GET / redirects to /dashboard", async () => {
    const r = await http("root", "/");
    expect([301, 302, 307, 308].includes(r.res.status), `status ${r.res.status}`);
    expect((r.res.headers.get("location") || "").includes("/dashboard"), `location ${r.res.headers.get("location")}`);
  });
  await check("P3", "GET /dashboard without a session redirects to /login", async () => {
    const r = await http("dashboard-anon", "/dashboard");
    expect([301, 302, 307, 308].includes(r.res.status), `status ${r.res.status}`);
    expect((r.res.headers.get("location") || "").includes("/login"), `location ${r.res.headers.get("location")}`);
  });
  let assetPath = "";
  await check("P4", "GET /login renders (200, HTML)", async () => {
    const r = await http("login-page", "/login");
    expect(r.res.status === 200, `status ${r.res.status}`);
    expect((r.res.headers.get("content-type") || "").includes("text/html"), "not html");
    const m = /\/_next\/static\/[A-Za-z0-9_\-./%]+\.js/.exec(r.text);
    if (m) assetPath = m[0];
  });
  await check("P5", "static asset is immutable and served", async () => {
    if (!assetPath) return { skip: "no /_next/static script found in /login HTML" };
    const r = await http("static", assetPath);
    expect(r.res.status === 200, `status ${r.res.status}`);
    const cc = r.res.headers.get("cache-control") || "";
    expect(/max-age=31536000/.test(cc) && /immutable/.test(cc), `cache-control "${cc}"`);
    return { detail: `cf-cache-status=${r.res.headers.get("cf-cache-status") || "n/a"}` };
  });
  await check("P6", "GET /api/auth/status is public and reports requireLogin", async () => {
    const r = await http("auth-status", "/api/auth/status");
    expect(r.res.status === 200, `status ${r.res.status}`);
    expect(r.json()?.requireLogin === true, "requireLogin is not true");
  });
  await check("P7", "GET /v1/models without a key is 401", async () => {
    const r = await http("models-nokey", "/v1/models");
    expect(r.res.status === 401, `status ${r.res.status}`);
  });
  await check("P8", "POST /v1/chat/completions with an invalid key is 401", async () => {
    const r = await http("chat-badkey", "/v1/chat/completions", { method: "POST", headers: jsonHeaders(bearer("sk-invalid-smoke-key-000000")), body: chatBody(CHAT_MODEL) });
    expect(r.res.status === 401, `status ${r.res.status}`);
  });
  await check("P9", "admin API rejects a missing / wrong token", async () => {
    const a = await http("admin-anon", `/api/admin/keys?ownerRef=${OWNER}`);
    const b = await http("admin-wrong", `/api/admin/keys?ownerRef=${OWNER}`, { headers: bearer("wrong-token-000000000000") });
    expect(a.res.status === 401 && b.res.status === 401, `statuses ${a.res.status}/${b.res.status}`);
  });
  await check("P10", "unauthenticated /api/keys and /api/providers are 401", async () => {
    const a = await http("keys-anon", "/api/keys");
    const b = await http("providers-anon", "/api/providers");
    expect(a.res.status === 401 && b.res.status === 401, `statuses ${a.res.status}/${b.res.status}`);
  });
  await check("P11", "/api/cron/flush access control", async () => {
    const anon = await http("cron-anon", "/api/cron/flush");
    if (CRON_SECRET) {
      expect(anon.res.status === 401, `without secret: status ${anon.res.status} (expected 401)`);
      const ok = await http("cron-secret", "/api/cron/flush", { headers: bearer(CRON_SECRET) });
      expect(ok.res.status === 200, `with secret: status ${ok.res.status}`);
      return { detail: "401 without secret, 200 with secret" };
    }
    if (anon.res.status === 200) return { warn: "CRON_SECRET is not enforced: /api/cron/flush is PUBLIC (200 without credentials)" };
    expect(anon.res.status === 401, `status ${anon.res.status}`);
    return { detail: "401 without a secret" };
  });
  if (OVERSIZE) {
    await check("P12", "oversized JSON body is 413 with a JSON error", async () => {
      const big = Buffer.alloc(21 * 1024 * 1024, 97);
      const r = await http("oversize", "/v1/chat/completions", { method: "POST", headers: jsonHeaders(bearer("sk-oversize-smoke-000000")), body: big });
      expect(r.res.status === 413, `status ${r.res.status}`);
      expect(r.json()?.error?.code === "request_too_large", `body ${r.text.slice(0, 120)}`);
      return { detail: `${Math.round(r.ms)} ms (includes the upload)` };
    });
  } else {
    record("P12", "oversized JSON body is 413 with a JSON error", "skip", "set SMOKE_OVERSIZE=1 (uploads 21 MB)");
  }
}

// Session cookie signed like src/lib/auth/dashboardSession.js: HS256 JWT {authenticated:true, iat, exp}, cookie `auth_token`.
function mintSessionCookie(secret) {
  const b64 = (o) => Buffer.from(typeof o === "string" ? o : JSON.stringify(o)).toString("base64url");
  const now = Math.floor(Date.now() / 1000);
  const head = b64({ alg: "HS256" });
  const body = b64({ authenticated: true, iat: now, exp: now + 3600 });
  const sig = crypto.createHmac("sha256", secret).update(`${head}.${body}`).digest("base64url");
  const token = `${head}.${body}.${sig}`;
  sessionSecrets.add(token);
  return `auth_token=${token}`;
}

let dashCookie = "";          // session used by the dashboard cases
let dashAuthMethod = "none";  // "password" | "jwt-cookie" | "none"
const dashKeyIds = [];        // keys created through the dashboard API, removed in cleanup
const dashApi = (label, p, init = {}) => http(label, p, { ...init, headers: { cookie: dashCookie, ...(init.headers || {}) } });

async function dashboardCases() {
  if (!PASSWORDS.length && !JWT_SECRET) {
    for (const [id, name] of [["D1", "dashboard login"], ["D2", "dashboard pages behind login"], ["D3", "logout"]]) record(id, name, "skip", "no dashboard password and no JWT secret provided");
    return;
  }
  let cookie = "";
  let loginDetail = "";
  let attempted = 0;
  let sessionOk = false;
  for (const candidate of PASSWORDS) {
    if (attempted > 0) await sleep(31000); // never hammer the login limiter
    attempted++;
    const r = await http("login", "/api/auth/login", { method: "POST", headers: jsonHeaders(), body: JSON.stringify({ password: candidate }) });
    if (r.res.status === 200) {
      cookie = (r.res.headers.getSetCookie?.() || []).map((c) => c.split(";")[0]).join("; ");
      if (cookie) { dashAuthMethod = "password"; loginDetail = `password #${attempted} accepted`; break; }
    }
    loginDetail = `password #${attempted} rejected (HTTP ${r.res.status})`;
    if (r.res.status === 429) break; // locked out: stop immediately
  }
  if (!cookie && JWT_SECRET) {
    cookie = mintSessionCookie(JWT_SECRET);
    dashAuthMethod = "jwt-cookie";
    loginDetail = `${loginDetail ? loginDetail + "; " : ""}fell back to a locally signed session cookie (Worker JWT secret)`;
  }
  await check("D1", "dashboard authentication (password login, else signed session cookie)", async () => {
    expect(cookie, `no session: ${loginDetail || "no credentials"}`);
    // The cookie must actually be accepted by the Worker, otherwise the secret is not the Worker's.
    const probe = await http("session-probe", "/api/keys", { headers: { cookie } });
    sessionOk = probe.res.status === 200;
    expect(probe.res.status === 200, `session rejected by the Worker: GET /api/keys -> ${probe.res.status} (${loginDetail})`);
    return { detail: `method=${dashAuthMethod}; ${loginDetail}` };
  });
  if (!sessionOk) {
    for (const id of ["D2", "D3", "D4", "D5", "D6", "D7"]) record(id, "dashboard case", "skip", "no valid session");
    return;
  }
  dashCookie = cookie;
  await check("D2", "dashboard pages return 200 behind login", async () => {
    const bad = [];
    for (const p of ["/dashboard", "/dashboard/endpoint", "/dashboard/providers", "/dashboard/usage", "/dashboard/profile"]) {
      let r = await http("dash:" + p, p, { headers: { cookie } });
      if ([301, 302, 307, 308].includes(r.res.status)) {
        const loc = r.res.headers.get("location") || "";
        if (loc.includes("/login")) { bad.push(`${p} -> login`); continue; }
        r = await http("dash:" + p, new URL(loc, BASE).pathname, { headers: { cookie } });
      }
      if (r.res.status !== 200) bad.push(`${p} ${r.res.status}`);
    }
    expect(!bad.length, bad.join(", "));
  });
  await check("D4", "dashboard APIs behind login: /api/keys, /api/providers, /api/usage/*", async () => {
    const keys = await dashApi("dash-api:keys", "/api/keys");
    expect(keys.res.status === 200, `/api/keys ${keys.res.status}`);
    const providers = await dashApi("dash-api:providers", "/api/providers");
    expect(providers.res.status === 200, `/api/providers ${providers.res.status}`);
    const chart = await dashApi("dash-api:usage-chart", "/api/usage/chart?period=7d");
    expect(chart.res.status === 200, `/api/usage/chart ${chart.res.status}`);
    const usageKeys = await dashApi("dash-api:usage-keys", "/api/usage/api-keys?period=7d");
    expect(usageKeys.res.status === 200, `/api/usage/api-keys ${usageKeys.res.status}`);
    const conns = (providers.json()?.connections || []).map((c) => `${c.provider}/${c.name || c.id}:${c.testStatus || "?"}`);
    return { detail: `${(keys.json()?.keys || []).length} key(s); connections (names and test status only): ${conns.join(", ") || "none"}` };
  });
  await check("D5", "POST /api/keys: mismatched forceProvider/forceModel pair is 400, bare pin vs prefixed blocked entry is 400", async () => {
    const mismatch = await dashApi("dash-keys:post-mismatch", "/api/keys", { method: "POST", headers: jsonHeaders(), body: JSON.stringify({ name: `smoke-test-DELETE-ME-${STAMP}-mismatch`, forceProvider: OTHER_PROVIDER, forceModel: CHAT_MODEL }) });
    if (mismatch.res.status === 201) { const j = mismatch.json(); if (j?.id) dashKeyIds.push(j.id); }
    expect(mismatch.res.status === 400, `mismatched pair: status ${mismatch.res.status} ${mismatch.text.slice(0, 120)}`);
    // A "bare" pin has no slash, so a leaf like @cf/meta/llama is not bare; use a fixed bare pin. The conflict check runs before any
    // provider resolution, so the pair needs no real connection.
    const bare = await dashApi("dash-keys:post-blocked", "/api/keys", { method: "POST", headers: jsonHeaders(), body: JSON.stringify({ name: `smoke-test-DELETE-ME-${STAMP}-blocked`, forceModel: "gpt-4o", blockedModels: ["openai/gpt-4o"] }) });
    if (bare.res.status === 201) { const j = bare.json(); if (j?.id) dashKeyIds.push(j.id); }
    expect(bare.res.status === 400, `bare pin covered by prefixed blocked entry: status ${bare.res.status} ${bare.text.slice(0, 120)}`);
    return { detail: `mismatch -> 400 "${String(mismatch.json()?.error || "").slice(0, 90)}"` };
  });
  await check("D6", "dashboard key CRUD: create, PUT mismatch is 400, PUT valid is 200, listed, deleted", async () => {
    const chatProvider = CHAT_PROVIDER;
    const created = await dashApi("dash-keys:create", "/api/keys", { method: "POST", headers: jsonHeaders(), body: JSON.stringify({ name: `smoke-test-DELETE-ME-${STAMP}-dash`, forceProvider: chatProvider, forceModel: CHAT_MODEL }) });
    expect(created.res.status === 201, `create ${created.res.status} ${created.text.slice(0, 120)}`);
    const id = created.json()?.id;
    expect(id, "no id in create response");
    dashKeyIds.push(id);
    createdKeySecrets.add(created.json()?.key);
    const badPut = await dashApi("dash-keys:put-mismatch", `/api/keys/${encodeURIComponent(id)}`, { method: "PUT", headers: jsonHeaders(), body: JSON.stringify({ forceProvider: OTHER_PROVIDER }) });
    expect(badPut.res.status === 400, `PUT mismatched pair: status ${badPut.res.status} ${badPut.text.slice(0, 120)}`);
    const okPut = await dashApi("dash-keys:put", `/api/keys/${encodeURIComponent(id)}`, { method: "PUT", headers: jsonHeaders(), body: JSON.stringify({ description: "smoke test" }) });
    expect(okPut.res.status === 200, `PUT valid: status ${okPut.res.status} ${okPut.text.slice(0, 120)}`);
    const list = await dashApi("dash-keys:list", "/api/keys");
    expect((list.json()?.keys || []).some((k) => k.id === id), "created key is not listed");
    const del = await dashApi("dash-keys:delete", `/api/keys/${encodeURIComponent(id)}`, { method: "DELETE" });
    expect([200, 204].includes(del.res.status), `DELETE status ${del.res.status}`);
    dashKeyIds.splice(dashKeyIds.indexOf(id), 1);
    const after = await dashApi("dash-keys:list", "/api/keys");
    expect(!(after.json()?.keys || []).some((k) => k.id === id), "deleted key is still listed");
  });
  await check("D3", "logout clears the session cookie", async () => {
    const r = await http("logout", "/api/auth/logout", { method: "POST", headers: { cookie } });
    expect(r.res.status === 200, `status ${r.res.status}`);
  });
}

// Analytics flush: force the buffer to D1 through the cron route and read the usage endpoints back.
async function analyticsFlushCase() {
  await check("A1", "POST /api/cron/flush flushes the analytics buffer; usage endpoints answer afterwards", async () => {
    const headers = CRON_SECRET ? bearer(CRON_SECRET) : {};
    const r = await http("cron-flush", "/api/cron/flush", { method: "POST", headers });
    expect(r.res.status === 200, `status ${r.res.status} ${r.text.slice(0, 100)}`);
    const j = r.json();
    expect(j?.ok === true, `body ${r.text.slice(0, 120)}`);
    let usage = "usage endpoints not read (no dashboard session)";
    if (dashCookie) {
      const chart = await dashApi("dash-api:usage-chart", "/api/usage/chart?period=today");
      expect(chart.res.status === 200, `usage chart ${chart.res.status}`);
      const keysUsage = await dashApi("dash-api:usage-keys", "/api/usage/api-keys?period=today");
      expect(keysUsage.res.status === 200, `usage api-keys ${keysUsage.res.status}`);
      const points = Array.isArray(chart.json()) ? chart.json() : (chart.json()?.data || chart.json()?.chart || []);
      usage = `chart(today) ${Array.isArray(points) ? points.length : "?"} point(s)`;
    }
    return { detail: `buffering=${j.buffering} analytics before=${j.analytics?.before} after=${j.analytics?.after}; ${usage}` };
  });
}

async function gatewayCases() {
  if (!ADMIN_TOKEN && !GIVEN_KEY) {
    record("G0", "gateway cases", "skip", "neither SMOKE_ADMIN_TOKEN nor SMOKE_API_KEY provided");
    return;
  }
  let key = GIVEN_KEY;
  if (!key) {
    const created = await createKey("main", { allowedModels: [CHAT_MODEL], rateLimitRequests: 600, rateLimitWindowSeconds: 60 });
    await check("G1", "admin API creates a throwaway key", async () => {
      expect(created.status === 201, `status ${created.status} ${JSON.stringify(created.body).slice(0, 120)}`);
      key = created.key;
    });
  } else {
    record("G1", "admin API creates a throwaway key", "skip", "using SMOKE_API_KEY");
  }
  if (!key) return;

  await check("G2", "GET /v1/models with key: 200 + ETag, If-None-Match -> 304", async () => {
    const a = await whenVisible("main", key, () => http("models", "/v1/models", { headers: bearer(key) }));
    expect(a.res.status === 200, `status ${a.res.status} ${a.text.slice(0, 100)}`);
    const etag = a.res.headers.get("etag");
    expect(etag, "no ETag header");
    const ids = (a.json()?.data || []).map((m) => m.id);
    const b = await http("models-304", "/v1/models", { headers: { ...bearer(key), "if-none-match": etag } });
    if (b.res.status !== 304 && /^W\//.test(etag)) {
      // Known: Cloudflare's edge weakens the ETag when it compresses the body, and the app compares strictly.
      const strong = await http("models-304", "/v1/models", { headers: { ...bearer(key), "if-none-match": etag.replace(/^W\//, "") } });
      if (strong.res.status === 304) return { warn: `edge sent weak ETag ${etag.slice(0, 12)}…; echoing it back gives ${b.res.status}, only the strong form gives 304 (strict If-None-Match comparison)` };
    }
    expect(b.res.status === 304, `conditional status ${b.res.status}`);
    return { detail: `${ids.length} model(s) visible to this key` };
  });

  // --- real provider calls (few) ---
  const realAllowed = () => realCalls < MAX_REAL;
  await check("C1", "chat non-stream (max_tokens 16) through the real provider", async () => {
    if (!realAllowed()) return { skip: "SMOKE_MAX_REAL_CALLS reached" };
    realCalls++;
    const r = await whenVisible("main", key, () => http("chat", "/v1/chat/completions", { method: "POST", headers: jsonHeaders(bearer(key)), body: chatBody(CHAT_MODEL, { stream: false }) }));
    if ([402, 403, 429, 502, 503].includes(r.res.status)) {
      return { skip: `upstream/credential problem HTTP ${r.res.status}: ${r.text.slice(0, 120)}` };
    }
    expect(r.res.status === 200, `status ${r.res.status} ${r.text.slice(0, 160)}`);
    const j = r.json();
    expect(Array.isArray(j?.choices) && j.choices.length > 0, "no choices[]");
    return { detail: `${Math.round(r.ms)} ms, usage=${JSON.stringify(j.usage || {})}` };
  });
  await check("C2", "chat stream (SSE) through the real provider", async () => {
    if (!realAllowed()) return { skip: "SMOKE_MAX_REAL_CALLS reached" };
    realCalls++;
    const started = performance.now();
    const res = await fetch(BASE + "/v1/chat/completions", { method: "POST", headers: { ...baseHeaders, ...jsonHeaders(bearer(key)) }, body: chatBody(CHAT_MODEL, { stream: true }) });
    if ([402, 403, 429, 502, 503].includes(res.status)) return { skip: `upstream/credential problem HTTP ${res.status}` };
    expect(res.status === 200, `status ${res.status}`);
    expect((res.headers.get("content-type") || "").includes("text/event-stream"), `content-type ${res.headers.get("content-type")}`);
    const reader = res.body.getReader();
    const dec = new TextDecoder();
    let text = "", firstByte = null;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (firstByte === null) firstByte = performance.now() - started;
      text += dec.decode(value, { stream: true });
    }
    const total = performance.now() - started;
    samples.push({ label: "chat-stream", ms: total, status: res.status });
    const chunks = (text.match(/^data:/gm) || []).length;
    const done = /data:\s*\[DONE\]/.test(text);
    const finished = /"finish_reason"\s*:\s*"(stop|length|tool_calls)"/.test(text);
    expect(done || finished, "stream ended without [DONE] and without a finish_reason chunk");
    const detail = `ttfb ${Math.round(firstByte)} ms, total ${Math.round(total)} ms, ${chunks} events`;
    if (!done) return { warn: `${detail}; finish_reason present but no "data: [DONE]" sentinel (OpenAI SDKs tolerate this, some clients do not)` };
    return { detail };
  });

  if (!ADMIN_TOKEN) {
    for (const id of ["K1", "K2", "K3", "K4", "K5", "K6"]) record(id, "key policy case", "skip", "needs SMOKE_ADMIN_TOKEN");
    return;
  }

  // --- key policy enforcement ---
  await check("K1", "forceModel pin rewrites the requested model (one real call)", async () => {
    if (!realAllowed()) return { skip: "SMOKE_MAX_REAL_CALLS reached" };
    const k = await createKey("pin-model", { forceModel: CHAT_MODEL });
    expect(k.status === 201, `create ${k.status} ${JSON.stringify(k.body).slice(0, 100)}`);
    realCalls++;
    const r = await whenVisible("pin-model", k.key, () => http("chat-pin", "/v1/chat/completions", { method: "POST", headers: jsonHeaders(bearer(k.key)), body: chatBody(OTHER_MODEL, { stream: false }) }));
    if ([402, 429, 502, 503].includes(r.res.status)) return { skip: `upstream problem HTTP ${r.res.status}` };
    expect(r.res.status === 200, `status ${r.res.status} ${r.text.slice(0, 120)} (a pinned key asking for another model must be rewritten, not denied)`);
    const served = String(r.json()?.model || "");
    expect(served.includes(MODEL_LEAF), `served model "${served}", expected the pinned ${MODEL_LEAF}`);
    return { detail: `asked ${OTHER_MODEL}, served ${served}` };
  });
  await check("K2", "forceProvider pin denies a model of another provider (403, no upstream call)", async () => {
    const k = await createKey("pin-provider", { forceProvider: OTHER_PROVIDER });
    expect(k.status === 201, `create ${k.status} ${JSON.stringify(k.body).slice(0, 100)}`);
    const r = await whenVisible("pin-provider", k.key, () => http("chat-pin-provider", "/v1/chat/completions", { method: "POST", headers: jsonHeaders(bearer(k.key)), body: chatBody(CHAT_MODEL) }));
    expect(r.res.status === 403, `status ${r.res.status} ${r.text.slice(0, 120)}`);
  });
  await check("K3", "allowedModels ACL denies a model outside the list (403)", async () => {
    const r = await whenVisible("main", key, () => http("chat-acl", "/v1/chat/completions", { method: "POST", headers: jsonHeaders(bearer(key)), body: chatBody(OTHER_MODEL) }));
    if (GIVEN_KEY) return { skip: "SMOKE_API_KEY has an unknown ACL" };
    expect(r.res.status === 403, `status ${r.res.status} ${r.text.slice(0, 120)}`);
  });
  await check("K4", "blockedModels denies the model (403)", async () => {
    const k = await createKey("blocked", { blockedModels: [CHAT_MODEL] });
    expect(k.status === 201, `create ${k.status}`);
    const r = await whenVisible("blocked", k.key, () => http("chat-blocked", "/v1/chat/completions", { method: "POST", headers: jsonHeaders(bearer(k.key)), body: chatBody(CHAT_MODEL) }));
    expect(r.res.status === 403, `status ${r.res.status} ${r.text.slice(0, 120)}`);
  });
  await check("K5", "admin API rejects a pin that conflicts with blockedModels (400)", async () => {
    const k = await createKey("conflict", { forceModel: CHAT_MODEL, blockedModels: [CHAT_MODEL] });
    expect(k.status === 400, `status ${k.status} ${JSON.stringify(k.body).slice(0, 120)}`);
  });
  await check("K6", "rate limit: 3 per 60 s, 4th request is 429 with Retry-After (no upstream call)", async () => {
    const k = await createKey("ratelimit", { rateLimitRequests: 3, rateLimitWindowSeconds: 60 });
    expect(k.status === 201, `create ${k.status}`);
    const statuses = [];
    let retryAfter = null;
    for (let i = 0; i < 5; i++) {
      // A model that resolves nowhere: the request passes the key checks (and is counted) but never reaches a provider.
      const send = () => http("chat-ratelimit", "/v1/chat/completions", { method: "POST", headers: jsonHeaders(bearer(k.key)), body: chatBody("smoke-nonexistent/none") });
      // Every request waits for visibility: consecutive requests can land on different isolates, and one that has not seen the key yet answers 401 (not counted).
      const r = await whenVisible("ratelimit", k.key, send);
      statuses.push(r.res.status);
      if (r.res.status === 429) retryAfter = r.res.headers.get("retry-after");
      await new Promise((r2) => setTimeout(r2, 1000));
    }
    const first429 = statuses.indexOf(429);
    expect(first429 === 3, `statuses ${statuses.join(",")} (expected the first 429 at request 4)`);
    return { detail: `statuses ${statuses.join(",")}, Retry-After=${retryAfter}` };
  });
  await check("K7", "a disabled key is 401", async () => {
    const k = await createKey("disabled", { isActive: false });
    expect(k.status === 201, `create ${k.status}`);
    let r;
    for (let attempt = 0; attempt < 5; attempt++) {
      r = await http("chat-disabled", "/v1/chat/completions", { method: "POST", headers: jsonHeaders(bearer(k.key)), body: chatBody(CHAT_MODEL) });
      if (r.res.status !== 503 || (r.res.headers.get("content-type") || "").includes("json")) break;
      cpuKills++; // Cloudflare CPU-limit kill, the app did not decide: retry
      await sleep(1500);
    }
    expect(r.res.status === 401, `status ${r.res.status}`);
  });
}

async function soak() {
  if (!SOAK_SECONDS || !GIVEN_KEY && !createdKeys.length) return;
  const key = GIVEN_KEY || createdKeys[0].key;
  log(`soak: ${SOAK_SECONDS}s, one request every ${SOAK_INTERVAL} ms (mixed health / models / models-304 / rejected chat)`);
  const end = Date.now() + SOAK_SECONDS * 1000;
  let etag = "";
  let i = 0;
  let errors = 0;
  while (Date.now() < end) {
    const turn = i++ % 4;
    try {
      let r;
      if (turn === 0) r = await http("soak:health", "/api/health");
      else if (turn === 1) { r = await http("soak:models", "/v1/models", { headers: bearer(key) }); etag = r.res.headers.get("etag") || etag; }
      else if (turn === 2) r = await http("soak:models-304", "/v1/models", { headers: { ...bearer(key), ...(etag ? { "if-none-match": etag } : {}) } });
      else r = await http("soak:chat-denied", "/v1/chat/completions", { method: "POST", headers: jsonHeaders(bearer("sk-invalid-smoke-key-000000")), body: chatBody(CHAT_MODEL) });
      if (r.res.status >= 500) errors++;
    } catch (e) {
      errors++;
      log("soak request failed:", e.message);
    }
    await new Promise((r) => setTimeout(r, SOAK_INTERVAL));
  }
  record("S1", `soak ${SOAK_SECONDS}s (${i} requests)`, errors === 0 ? "pass" : "fail", `${errors} request(s) with a 5xx / network error`);
}

async function cleanup() {
  if (dashCookie && dashKeyIds.length && !KEEP_KEYS) {
    for (const id of dashKeyIds.splice(0)) {
      for (let attempt = 0; attempt < 4; attempt++) {
        const r = await dashApi("dash-keys:delete", `/api/keys/${encodeURIComponent(id)}`, { method: "DELETE" }).catch(() => null);
        if (r && [200, 204, 404].includes(r.res.status)) break;
        await sleep(2000 * (attempt + 1));
      }
    }
  }
  if (!createdKeys.length) return;
  if (KEEP_KEYS) { record("X1", "throwaway keys deleted", "skip", "SMOKE_KEEP_KEYS=1"); return; }
  await check("X1", "throwaway keys deleted and gone from the key list", async () => {
    const listSmokeKeys = async () => {
      for (let attempt = 0; attempt < 6; attempt++) {
        const list = await http("admin:list-keys", `/api/admin/keys?ownerRef=${OWNER}`, { headers: adminHeaders() });
        if (list.res.status === 200) return (list.json()?.keys || []).filter((k) => String(k.name || "").startsWith("smoke-test-DELETE-ME-"));
        await sleep(2000 * (attempt + 1)); // a 503 is the Worker CPU limit; retry on a fresh invocation
      }
      throw new Error("could not list keys");
    };
    // Delete what we created plus any straggler of this dedicated owner left by an interrupted earlier run.
    const ids = new Set(createdKeys.map((k) => k.id));
    for (const k of await listSmokeKeys()) ids.add(k.id);
    const failures = [];
    for (const id of ids) {
      let r;
      for (let attempt = 0; attempt < 6; attempt++) {
        r = await deleteKey(id);
        if ([200, 204, 404].includes(r.res.status)) break;
        await sleep(2000 * (attempt + 1));
      }
      if (![200, 204, 404].includes(r.res.status)) failures.push(`${id} -> ${r.res.status}`);
    }
    const left = await listSmokeKeys();
    expect(!failures.length, failures.join(", "));
    expect(left.length === 0, `${left.length} smoke key(s) still listed`);
    return { detail: `${ids.size} deleted` };
  });
}

// ---------- main ----------
log(`smoke target ${BASE}${VERSION_OVERRIDE ? "  version override " + VERSION_OVERRIDE.slice(0, 8) : ""}  (admin token: ${ADMIN_TOKEN ? "set" : "no"}, dashboard password: ${PASSWORDS.length ? PASSWORDS.length + " candidate(s)" : "no"}, jwt secret: ${JWT_SECRET ? "set" : "no"}, api key: ${GIVEN_KEY ? "given" : "throwaway"})`);
try {
  await publicCases();
  await dashboardCases();
  await gatewayCases();
  await analyticsFlushCase();
  await soak();
} finally {
  await cleanup();
}

const byLabel = {};
for (const s of samples) (byLabel[s.label] ||= []).push(s);
const latency = Object.fromEntries(Object.entries(byLabel).map(([k, v]) => [k, summarize(v)]));
log("\nclient-side latency (ms, nearest-rank percentiles; measured from this machine, not a benchmark):");
for (const [label, s] of Object.entries(latency)) {
  if (s.n >= 1) log(`  ${label.padEnd(22)} n=${String(s.n).padStart(3)}  p50=${s.p50.toFixed(0).padStart(5)}  p95=${s.p95.toFixed(0).padStart(5)}  max=${s.max.toFixed(0).padStart(5)}`);
}
const tally = cases.reduce((a, c) => ((a[c.status] = (a[c.status] || 0) + 1), a), {});
if (visibility.length) log(`new-key visibility delays (401 until visible to the serving isolate): ${visibility.map((v) => `${v.label}=${v.seconds}s`).join(", ")}`);
if (cpuKills) log(`retried ${cpuKills} policy request(s) that Cloudflare answered with a non-JSON 503 (CPU limit kill) before the app could decide`);
log(`\nresult: ${JSON.stringify(tally)}  requests=${samples.length}  realProviderCalls=${realCalls}`);
if (REPORT) {
  fs.writeFileSync(REPORT, redact(JSON.stringify({ base: BASE, at: new Date().toISOString(), cases, latency, visibility, samples: samples.map((s) => ({ ...s, ms: Math.round(s.ms * 10) / 10 })), realCalls }, null, 2)));
  log(`report written to ${REPORT}`);
}
process.exit(tally.fail ? 1 : 0);
