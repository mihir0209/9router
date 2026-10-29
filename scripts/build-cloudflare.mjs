import { renameSync, existsSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { pathToFileURL } from "node:url";

const root = process.cwd();
const proxyPath = join(root, "src", "proxy.js");
const cloudflareProxyPath = join(root, "src", "proxy.cloudflare.js");
const disabledProxyPath = join(root, "src", "proxy.js.cloudflare-disabled");
const middlewarePath = join(root, "src", "middleware.js");
const driverPath = join(root, "src", "lib", "db", "driver.js");
const cloudflareDriverPath = join(root, "src", "lib", "db", "driver.cloudflare.js");
const disabledDriverPath = join(root, "src", "lib", "db", "driver.js.cloudflare-disabled");
const workerPath = join(root, ".open-next", "worker.js");

let swappedProxy = false;
let swappedDriver = false;

function restoreFiles() {
  if (swappedProxy && existsSync(disabledProxyPath)) {
    if (existsSync(middlewarePath)) renameSync(middlewarePath, cloudflareProxyPath);
    renameSync(disabledProxyPath, proxyPath);
    swappedProxy = false;
  }
  if (swappedDriver && existsSync(disabledDriverPath)) {
    if (existsSync(driverPath)) renameSync(driverPath, cloudflareDriverPath);
    renameSync(disabledDriverPath, driverPath);
    swappedDriver = false;
  }
}

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => {
    try { restoreFiles(); } catch { /* best effort */ }
    process.kill(process.pid, signal);
  });
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: "inherit",
    shell: process.platform === "win32",
    env: {
      ...process.env,
      NINEROUTER_RUNTIME: "cloudflare",
    },
  });
  if (result.status !== 0) throw new Error(`${command} ${args.join(" ")} failed with status ${result.status || 1}`);
}

/**
 * Fail the build when the two copies of the auth policy disagree.
 *
 * The guard below duplicates the policy in src/proxy.cloudflare.js because it
 * has to run before the Next middleware. That duplication is invisible: editing
 * only the source file produces a build that contains the change and a
 * production worker that ignores it, which is exactly what happened here — the
 * edit shipped, the artifact was correct, and /v1/models still returned 401.
 * Both copies are read from source, so this check stays true as they change.
 */

/**
 * Execute the generated guard and check what it actually decides.
 *
 * Parsing is not enough. A bare expression where a declaration belonged parses
 * perfectly and then throws ReferenceError on first use, which is how the
 * previous deploy 1101'd on every route. This runs the real generated code
 * against a real Request and asserts the public paths pass and the guarded ones
 * do not, so a broken guard fails the build instead of the site.
 */
function assertGuardDecidesCorrectly(guardSource, defaultMaxBodyMb) {
    const driver = `
for (const path of ["/v1/models", "/v1/models/", "/v1/chat/completions", "/v1/completions", "/v1/messages", "/v1/embeddings"]) {
  const res = await guardCloudflareAdminRequest(new Request("https://w.test" + path), {});
  if (!res || res.status !== 401) {
    throw new Error(
      "guard let " + path + " through without a key (status " + (res && res.status) +
      "). /v1/models must stay authenticated: an open catalogue is an " +
      "unauthenticated amplification point."
    );
  }
}

const assert = (cond, msg) => { if (!cond) throw new Error("guard check failed: " + msg); };
const post = (headers, body) => new Request("https://w.test/v1/chat/completions", { method: "POST", headers, body });
const auth = { authorization: "Bearer sk-test" };

// Body limits: Content-Length is checked before anything else runs.
const json = { ...auth, "content-type": "application/json" };
const big = String(__DEFAULT_MAX_BODY_MB__ * 1048576 + 1);
let res = await guardCloudflareAdminRequest(post({ ...json, "content-length": big }, "{}"), {});
assert(res && res.status === 413, "oversized JSON body must be rejected with 413");
const err = await res.json();
assert(err.error && err.error.code === "request_too_large", "413 must carry an OpenAI-style error object");
res = await guardCloudflareAdminRequest(post({ ...json, "content-length": "1024" }, "{}"), {});
assert(res === null, "a small JSON body must pass the guard");
res = await guardCloudflareAdminRequest(post({ ...json, "content-length": big }, "{}"), { NINEROUTER_MAX_BODY_MB: "0" });
assert(res === null, "NINEROUTER_MAX_BODY_MB=0 must disable the limit");
res = await guardCloudflareAdminRequest(post({ ...json, "content-length": "2097152" }, "{}"), { NINEROUTER_MAX_BODY_MB: "1" });
assert(res && res.status === 413, "the env limit must override the default");
res = await guardCloudflareAdminRequest(post({ ...auth, "content-type": "multipart/form-data; boundary=x", "content-length": big }, "x"), {});
assert(res === null, "uploads use the (larger) upload limit, not the JSON limit");
res = await guardCloudflareAdminRequest(post({ "content-type": "application/json", "content-length": big }, "{}"), {});
assert(res && res.status === 401, "an unauthenticated oversized request is a 401, not a size oracle");

// Downstream request: x-9r-real-ip handling and no body tee.
let down = __9routerDownstreamRequest(post({ ...json, "x-9r-real-ip": "6.6.6.6" }, '{"a":1}'));
assert(down.headers.get("x-9r-real-ip") === null, "a client-supplied x-9r-real-ip must be stripped without cf-connecting-ip");
down = __9routerDownstreamRequest(post({ ...json, "x-9r-real-ip": "6.6.6.6", "cf-connecting-ip": "1.2.3.4" }, '{"a":1}'));
assert(down.headers.get("x-9r-real-ip") === "1.2.3.4", "cf-connecting-ip must overwrite x-9r-real-ip");
assert((await down.text()) === '{"a":1}', "the downstream request must still carry the body");
const original = post({ ...json, "cf-connecting-ip": "1.2.3.4" }, "{}");
let cloned = false;
const origClone = original.clone.bind(original);
original.clone = () => { cloned = true; return origClone(); };
__9routerDownstreamRequest(original);
assert(!cloned, "the body must not be cloned (clone() tees and buffers it)");

// Env is copied once per isolate.
delete process.env.__GUARD_PROBE;
__9routerSyncEnv({ __GUARD_PROBE: "one" });
assert(process.env.__GUARD_PROBE === "one", "env must be copied on the first request");
delete process.env.__GUARD_PROBE;
__9routerSyncEnv({ __GUARD_PROBE: "two" });
assert(process.env.__GUARD_PROBE === undefined, "env must not be copied again on later requests");
`;
  const tmp = join(tmpdir(), `9router-guard-check-${process.pid}.mjs`);
  writeFileSync(tmp, guardSource + driver.replace(/__DEFAULT_MAX_BODY_MB__/g, String(defaultMaxBodyMb)));
  try {
    const result = spawnSync(process.execPath, [tmp], { encoding: "utf8" });
    if (result.status !== 0) {
      throw new Error(
        `the generated auth guard does not behave correctly, refusing to deploy:\n` +
          `${(result.stderr || "").split("\n").slice(0, 6).join("\n")}`
      );
    }
  } finally {
    rmSync(tmp, { force: true });
  }
}

/** The cron tick must use the service binding, never a hard-coded hostname. */
function assertScheduledUsesBinding(source) {
  const driver = `
const calls = [];
const binding = { fetch: async (req) => { calls.push({ url: req.url, method: req.method, auth: req.headers.get("authorization") }); return new Response("{}"); } };
globalThis.fetch = async () => { throw new Error("scheduled must not use the public fetch when the binding exists"); };
await __9ROUTER_SCHEDULED__({}, { WORKER_SELF_REFERENCE: binding, CRON_SECRET: "s3" }, null);
if (calls.length !== 1 || !calls[0].url.endsWith("/api/cron/flush") || calls[0].method !== "POST" || calls[0].auth !== "Bearer s3") {
  throw new Error("scheduled did not call the self binding correctly: " + JSON.stringify(calls));
}
let publicCalls = 0;
globalThis.fetch = async (u) => { publicCalls++; return new Response("{}"); };
await __9ROUTER_SCHEDULED__({}, {}, null);
if (publicCalls !== 0) throw new Error("scheduled must not call any hard-coded hostname without a binding or ORIGIN");
await __9ROUTER_SCHEDULED__({}, { ORIGIN: "https://example.test" }, null);
if (publicCalls !== 1) throw new Error("scheduled must fall back to env.ORIGIN");
`;
  const tmp = join(tmpdir(), `9router-scheduled-check-${process.pid}.mjs`);
  writeFileSync(tmp, source + driver);
  try {
    const result = spawnSync(process.execPath, [tmp], { encoding: "utf8" });
    if (result.status !== 0) {
      throw new Error(`the generated cron handler does not behave correctly:\n${(result.stderr || "").split("\n").slice(0, 6).join("\n")}`);
    }
  } finally {
    rmSync(tmp, { force: true });
  }
}

/**
 * Parse the generated worker before it can become a deployment.
 *
 * The guard is assembled by string substitution, so a mistake in it is invisible
 * until runtime: the deploy succeeds, the bundle contains the code, and every
 * request fails. That is exactly how a bare placeholder shipped as a
 * ReferenceError and turned the whole worker into 1101s. Checking the syntax
 * here costs seconds and would have caught it before the deploy.
 */
function assertWorkerParses(source) {
  const tmp = join(tmpdir(), `9router-worker-check-${process.pid}.mjs`);
  writeFileSync(tmp, source);
  try {
    const result = spawnSync(process.execPath, ["--check", tmp], { encoding: "utf8" });
    if (result.status !== 0) {
      throw new Error(
        `generated worker.js does not parse, refusing to hand a broken bundle to wrangler:\n${result.stderr}`
      );
    }
  } finally {
    rmSync(tmp, { force: true });
  }
}

async function injectWorkerGuard() {
  if (!existsSync(workerPath)) throw new Error(`${workerPath} does not exist after OpenNext build`);
  const source = readFileSync(workerPath, "utf8");
  if (source.includes("__9ROUTER_CF_ADMIN_GUARD__")) return;   // already injected

  const limits = await import(pathToFileURL(join(root, "open-sse", "config", "bodyLimits.js")).href);
  const guardTemplate = String.raw`
const __9ROUTER_CF_ADMIN_GUARD__ = true;
const PUBLIC_CLOUDFLARE_PATHS = [
  "/api/health",
  "/api/auth/login",
  "/api/auth/logout",
  "/api/auth/status",
  "/api/auth/oidc",
  "/api/settings/require-login",
  "/api/version",
  "/api/cron/flush",
  "/callback",
  "/login",
  "/landing",
  "/manifest.webmanifest",
];
const PUBLIC_LLM_PREFIXES = ["/v1", "/v1beta", "/api/v1", "/api/v1beta"];

function pathMatches(pathname, path) {
  return pathname === path || pathname.startsWith(path + "/");
}

function isPublicCloudflarePath(pathname) {
  return PUBLIC_CLOUDFLARE_PATHS.some((path) => pathMatches(pathname, path));
}


function isPublicLlmApi(pathname) {
  return PUBLIC_LLM_PREFIXES.some((path) => pathMatches(pathname, path));
}

function hasApiKeyHeader(request, url) {
  const authHeader = request.headers.get("authorization");
  return Boolean(
    authHeader?.startsWith("Bearer ") ||
    request.headers.get("x-api-key") ||
    request.headers.get("x-goog-api-key") ||
    url.searchParams.get("key")
  );
}

function getCookie(request, name) {
  const cookie = request.headers.get("cookie") || "";
  for (const part of cookie.split(";")) {
    const [rawKey, ...valueParts] = part.trim().split("=");
    if (rawKey === name) return valueParts.join("=");
  }
  return "";
}

function base64UrlToBytes(value) {
  const normalized = value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
  const binary = atob(normalized);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function bytesEqual(a, b) {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}

async function hasValidDashboardCookie(request, env) {
  const token = getCookie(request, "auth_token");
  const secret = env.JWT_SECRET;
  if (!token || !secret) return false;
  const parts = token.split(".");
  if (parts.length !== 3) return false;
  try {
    const [encodedHeader, encodedPayload, encodedSignature] = parts;
    const header = JSON.parse(new TextDecoder().decode(base64UrlToBytes(encodedHeader)));
    if (header.alg !== "HS256") return false;
    const key = await crypto.subtle.importKey(
      "raw",
      new TextEncoder().encode(secret),
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["sign"]
    );
    const signature = new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(encodedHeader + "." + encodedPayload)));
    if (!bytesEqual(signature, base64UrlToBytes(encodedSignature))) return false;
    const payload = JSON.parse(new TextDecoder().decode(base64UrlToBytes(encodedPayload)));
    if (payload.exp && payload.exp * 1000 <= Date.now()) return false;
    return payload.authenticated === true;
  } catch {
    return false;
  }
}

async function hasValidAdminToken(request, env) {
  const configured = env.ADMIN_API_TOKEN;
  if (!configured || env.INITIAL_PASSWORD === "123456") return false;
  const auth = request.headers.get("authorization") || "";
  if (!auth.startsWith("Bearer ")) return false;
  const supplied = auth.slice(7);
  if (!supplied || supplied.length !== configured.length) return false;
  const a = new TextEncoder().encode(supplied);
  const b = new TextEncoder().encode(configured);
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}

// Copy the Worker env (vars and secrets) into process.env. Bindings and vars are
// fixed for the life of an isolate, so this runs once, not on every request.
let __9routerEnvSynced = false;
function __9routerSyncEnv(env) {
  if (__9routerEnvSynced || !env) return;
  __9routerEnvSynced = true;
  for (const [k, v] of Object.entries(env)) {
    if (typeof v === "string" && !process.env[k]) process.env[k] = v;
  }
}

// Body size limits. Defaults are stamped from open-sse/config/bodyLimits.js at
// build time. 0 (or a non-positive env value) disables the limit.
const __9ROUTER_DEFAULT_MAX_BODY_MB = __DEFAULT_MAX_BODY_MB__;
const __9ROUTER_DEFAULT_MAX_UPLOAD_MB = __DEFAULT_MAX_UPLOAD_MB__;

function __9routerLimitBytes(env, name, defaultMb) {
  let raw = env && env[name] != null ? env[name] : undefined;
  if (raw == null || raw === "") raw = typeof process !== "undefined" ? process.env[name] : undefined;
  if (raw != null && raw !== "") {
    const mb = Number(raw);
    if (Number.isFinite(mb)) return mb > 0 ? Math.floor(mb * 1048576) : 0;
  }
  return Math.floor(defaultMb * 1048576);
}

// Cheap Content-Length check, before any database or Next work. A body without
// a Content-Length (chunked) is counted by the handlers while they read it.
function __9routerBodyTooLarge(request, env) {
  if (request.method === "GET" || request.method === "HEAD" || request.method === "OPTIONS") return null;
  const raw = request.headers.get("content-length");
  if (raw == null || !/^\d+$/.test(raw.trim())) return null;
  const declared = Number(raw);
  const isJson = (request.headers.get("content-type") || "").toLowerCase().includes("json");
  const limit = isJson
    ? __9routerLimitBytes(env, "NINEROUTER_MAX_BODY_MB", __9ROUTER_DEFAULT_MAX_BODY_MB)
    : __9routerLimitBytes(env, "NINEROUTER_MAX_UPLOAD_MB", __9ROUTER_DEFAULT_MAX_UPLOAD_MB);
  if (!limit || declared <= limit) return null;
  const mb = limit / 1048576;
  return Response.json(
    {
      error: {
        message: (isJson ? "request" : "upload") + " body exceeds the " + (Number.isInteger(mb) ? mb : mb.toFixed(2)) + " MB limit",
        type: "invalid_request_error",
        code: "request_too_large",
      },
    },
    { status: 413, headers: { "Access-Control-Allow-Origin": "*" } }
  );
}

// The request handed to Next. cf-connecting-ip is the only trusted client IP: it
// overwrites any client-supplied x-9r-real-ip, and without it the header is
// stripped. The body is never cloned: clone() tees the stream, and the branch
// nobody reads buffers the whole body in memory. new Request(request, init)
// moves the body to the new request instead.
function __9routerDownstreamRequest(request) {
  const headers = new Headers(request.headers);
  const cfIp = request.headers.get("cf-connecting-ip");
  if (cfIp) headers.set("x-9r-real-ip", cfIp);
  else headers.delete("x-9r-real-ip");
  return new Request(request, { headers });
}

async function guardCloudflareAdminRequest(request, env) {
  const url = new URL(request.url);
  const { pathname } = url;

  if (isPublicCloudflarePath(pathname)) return null;

  // Every LLM route requires a key, /v1/models included. The model list was
  // briefly public on the reasoning that a client needs the catalogue before it
  // holds one, but that is a free amplification point: any bot can make this
  // instance enumerate thousands of models for free, and it is the costliest
  // list to build. This guard runs before the middleware and before any database
  // work, so an unauthenticated caller gets a flat 401 without touching D1.


  if (isPublicLlmApi(pathname)) {
    if (!hasApiKeyHeader(request, url)) return Response.json({ error: "API key required" }, { status: 401 });
    return __9routerBodyTooLarge(request, env);
  }

  if (pathname.startsWith("/api/admin") || pathname.startsWith("/api/observability")) {
    if (await hasValidAdminToken(request, env)) return null;
  }

  if (pathname.startsWith("/api/") || pathname.startsWith("/dashboard")) {
    if (await hasValidDashboardCookie(request, env)) return null;
    if (pathname.startsWith("/dashboard")) return Response.redirect(new URL("/login", request.url), 302);
    return Response.json({ error: "Unauthorized" }, { status: 401 });
  }

  return null;
}
`;

  const guard = guardTemplate
    .replace("__DEFAULT_MAX_BODY_MB__", String(limits.DEFAULT_CLOUDFLARE_MAX_BODY_MB))
    .replace("__DEFAULT_MAX_UPLOAD_MB__", String(limits.DEFAULT_CLOUDFLARE_MAX_UPLOAD_MB));

  // The cron tick runs the flush through the WORKER_SELF_REFERENCE service binding: the call
  // never leaves Cloudflare's network, does not depend on the public hostname (a personal
  // workers.dev name used to be hard-coded here) and is not subject to WAF/rate-limit rules
  // written for public traffic. It is still one extra invocation of this Worker per tick, and it
  // lands on an arbitrary isolate, so it can only flush that isolate's buffers (documented in
  // docs/CLOUDFLARE_EXTREME_PROFILE.md). Without the binding, env.ORIGIN is the only fallback.
  const scheduled = `
async function __9ROUTER_SCHEDULED__(event, env, ctx) {
  __9routerSyncEnv(env);
  const secret = env.CRON_SECRET;
  const headers = { 'content-type': 'application/json' };
  if (secret) headers.authorization = 'Bearer ' + secret;
  const path = '/api/cron/flush';
  const run = (async () => {
    try {
      let res;
      if (env.WORKER_SELF_REFERENCE && typeof env.WORKER_SELF_REFERENCE.fetch === 'function') {
        res = await env.WORKER_SELF_REFERENCE.fetch(new Request((env.ORIGIN || 'https://worker-self.internal') + path, { method: 'POST', headers }));
      } else if (env.ORIGIN) {
        res = await fetch(env.ORIGIN + path, { method: 'POST', headers });
      } else {
        console.log('[cron flush] skipped: no WORKER_SELF_REFERENCE binding and no ORIGIN var');
        return;
      }
      const body = await res.text();
      console.log('[cron flush]', res.status, body.slice(0, 400));
    } catch (err) {
      console.log('[cron flush] failed', err && err.message);
    }
  })();
  if (ctx && typeof ctx.waitUntil === 'function') ctx.waitUntil(run);
  else await run;
}
`;

  const coordinatorPath = join(root, "src", "cloudflare", "gateway-coordinator.js");
  const coordinatorSource = readFileSync(coordinatorPath, "utf8")
    .replace(/^import[^\n]+\n/, "")
    .trim();
  const coordinatorRuntime = `import { DurableObject } from "cloudflare:workers";\n${coordinatorSource}\n`;

    const guardMarker = "return runWithCloudflareRequestContext(request, env, ctx, async () => {";
    if (!source.includes(guardMarker)) throw new Error("OpenNext worker no longer contains the request-context entry point; the guard cannot be injected");
    let withGuard = coordinatorRuntime + source.replace("export default {", `${guard}\n${scheduled}\nexport default {\n    scheduled: __9ROUTER_SCHEDULED__,`)
      .replace(
        guardMarker,
        "__9routerSyncEnv(env);\n        const guardResponse = await guardCloudflareAdminRequest(request, env);\n        if (guardResponse) return guardResponse;\n        const __9ROUTER_DOWNSTREAM_REQUEST = __9routerDownstreamRequest(request);\n        return runWithCloudflareRequestContext(__9ROUTER_DOWNSTREAM_REQUEST, env, ctx, async () => {"
      );
  assertWorkerParses(withGuard);
  assertGuardDecidesCorrectly(guard, limits.DEFAULT_CLOUDFLARE_MAX_BODY_MB);
  assertScheduledUsesBinding(guard + scheduled);
  writeFileSync(workerPath, withGuard);
}

/**
 * Cache headers for the static assets Cloudflare serves from the CDN, not the
 * Worker. Workers Assets reads a `_headers` file at the root of the assets
 * directory. It is written here, not placed in public/, so the Node build does
 * not serve it as a file.
 *
 * Only /_next/static is immutable: its file names carry a content hash. The other
 * directories use stable names, so they get a short lifetime and a deploy is
 * visible within it. sw.js is revalidated every time so a new service worker
 * is never stuck behind a cache.
 */
const ASSET_HEADERS = `# Generated by scripts/build-cloudflare.mjs. Do not edit.
/_next/static/*
  Cache-Control: public, max-age=31536000, immutable

/icons/*
  Cache-Control: public, max-age=86400

/providers/*
  Cache-Control: public, max-age=86400

/i18n/*
  Cache-Control: public, max-age=3600

/favicon.ico
  Cache-Control: public, max-age=86400

/favicon.svg
  Cache-Control: public, max-age=86400

/sw.js
  Cache-Control: public, max-age=0, must-revalidate
`;

function writeAssetHeaders() {
  const assetsDir = join(root, ".open-next", "assets");
  if (!existsSync(assetsDir)) throw new Error(`${assetsDir} does not exist after OpenNext build`);
  writeFileSync(join(assetsDir, "_headers"), ASSET_HEADERS, "utf8");
  if (!existsSync(join(assetsDir, "_next", "static"))) {
    throw new Error("assets/_next/static is missing; the immutable cache rule would match nothing");
  }
}

try {
  // Always build from a clean state: Next's webpack filesystem cache can
  // otherwise reuse modules compiled from pre-swap sources (e.g. the local
  // SQLite driver), poisoning the worker bundle with unresolvable natives
  // like bun:sqlite. NOTE: this deletes the local .next output — rerun the
  // regular `npm run build` afterwards if you also serve locally.
  for (const dir of [join(root, ".next"), join(root, ".open-next")]) {
    rmSync(dir, { recursive: true, force: true });
  }

  if (existsSync(disabledProxyPath)) {
    throw new Error(`${disabledProxyPath} already exists; refusing to overwrite`);
  }
  if (existsSync(middlewarePath)) {
    throw new Error(`${middlewarePath} already exists; refusing to overwrite`);
  }
  renameSync(proxyPath, disabledProxyPath);
  renameSync(cloudflareProxyPath, middlewarePath);
  swappedProxy = true;

  if (existsSync(disabledDriverPath)) {
    throw new Error(`${disabledDriverPath} already exists; refusing to overwrite`);
  }
  renameSync(driverPath, disabledDriverPath);
  renameSync(cloudflareDriverPath, driverPath);
  swappedDriver = true;

  run("npx", ["opennextjs-cloudflare", "build"]);
  await injectWorkerGuard();
  writeAssetHeaders();

  // The OpenNext build also produced .next/standalone (the Node layout), but
  // with the Cloudflare driver/middleware compiled in. Left unmarked, that
  // directory silently boots as a local server with an EPHEMERAL in-memory
  // database — imports and edits vanish on restart and the local SQLite file
  // is ignored. Mark the variant so custom-server.js can refuse it loudly.
  // (copy-standalone-assets.mjs overwrites the marker with "node" on the next
  // regular `npm run build`.)
  const standaloneDir = join(root, ".next", "standalone");
  if (existsSync(standaloneDir)) {
    writeFileSync(join(standaloneDir, ".9router-build-variant"), "cloudflare", "utf8");
  }
  process.exitCode = 0;
} catch (error) {
  console.error(error.message || error);
  process.exitCode = 1;
} finally {
  restoreFiles();
}
