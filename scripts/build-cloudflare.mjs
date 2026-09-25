import { renameSync, existsSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";

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

function injectWorkerGuard() {
  if (!existsSync(workerPath)) throw new Error(`${workerPath} does not exist after OpenNext build`);
  const source = readFileSync(workerPath, "utf8");
  if (source.includes("__9ROUTER_CF_ADMIN_GUARD__")) return;

  const guard = String.raw`
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

async function guardCloudflareAdminRequest(request, env) {
  const url = new URL(request.url);
  const { pathname } = url;

  if (isPublicCloudflarePath(pathname)) return null;

  if (isPublicLlmApi(pathname)) {
    if (hasApiKeyHeader(request, url)) return null;
    return Response.json({ error: "API key required" }, { status: 401 });
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

  const scheduled = `
async function __9ROUTER_SCHEDULED__(event, env, ctx) {
  if (env) { for (const [k, v] of Object.entries(env)) { if (typeof v === 'string' && !process.env[k]) process.env[k] = v; } }
  const secret = env.CRON_SECRET;
  const headers = { 'content-type': 'application/json' };
  if (secret) headers.authorization = 'Bearer ' + secret;
  const target = (env.ORIGIN || 'https://mihir-router.themihir.workers.dev') + '/api/cron/flush';
  const run = (async () => {
    try {
      const res = await fetch(target, { method: 'POST', headers });
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

  let withGuard = coordinatorRuntime + source.replace("export default {", `${guard}\n${scheduled}\nexport default {\n    scheduled: __9ROUTER_SCHEDULED__,`)
    .replace(
      "return runWithCloudflareRequestContext(request, env, ctx, async () => {",
      "if (env) { for (const [k, v] of Object.entries(env)) { if (typeof v === 'string' && !process.env[k]) process.env[k] = v; } }\n        const guardResponse = await guardCloudflareAdminRequest(request, env);\n        if (guardResponse) return guardResponse;\n        let __9ROUTER_DOWNSTREAM_REQUEST = request;\n        const __9ROUTER_CF_IP = request.headers.get('cf-connecting-ip');\n        if (__9ROUTER_CF_IP) { const __9ROUTER_HEADERS = new Headers(request.headers); __9ROUTER_HEADERS.set('x-9r-real-ip', __9ROUTER_CF_IP); __9ROUTER_DOWNSTREAM_REQUEST = new Request(request.clone(), { headers: __9ROUTER_HEADERS }); }\n        else { const __9ROUTER_HEADERS = new Headers(request.headers); __9ROUTER_HEADERS.delete('x-9r-real-ip'); __9ROUTER_DOWNSTREAM_REQUEST = new Request(request, { headers: __9ROUTER_HEADERS }); }\n        return runWithCloudflareRequestContext(__9ROUTER_DOWNSTREAM_REQUEST, env, ctx, async () => {"
    );
  writeFileSync(workerPath, withGuard);
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
  injectWorkerGuard();
  process.exitCode = 0;
} catch (error) {
  console.error(error.message || error);
  process.exitCode = 1;
} finally {
  restoreFiles();
}
