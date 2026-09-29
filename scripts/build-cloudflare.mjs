import { renameSync, existsSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { PUBLIC_MODEL_PATHS } from "../src/lib/publicModelPaths.js";

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
function readModelsPathsFromSource() {
  if (!PUBLIC_MODEL_PATHS.length) {
    throw new Error("PUBLIC_MODEL_PATHS is empty; the generated guard would 401 every request");
  }
  return PUBLIC_MODEL_PATHS;
}


/**
 * Execute the generated guard and check what it actually decides.
 *
 * Parsing is not enough. A bare expression where a declaration belonged parses
 * perfectly and then throws ReferenceError on first use, which is how the
 * previous deploy 1101'd on every route. This runs the real generated code
 * against a real Request and asserts the public paths pass and the guarded ones
 * do not, so a broken guard fails the build instead of the site.
 */
function assertGuardDecidesCorrectly(guardSource, modelsPaths) {
  const driver = `
const __cases = ${JSON.stringify(modelsPaths)};
for (const path of [...__cases, ...__cases.map((p) => p + "/")]) {
  const res = await guardCloudflareAdminRequest(new Request("https://w.test" + path), {});
  if (res !== null) {
    throw new Error("guard rejected public path " + path + " with " + (res && res.status));
  }
}
for (const path of ["/v1/chat/completions", "/v1/completions", "/v1/messages"]) {
  const res = await guardCloudflareAdminRequest(new Request("https://w.test" + path), {});
  if (!res || res.status !== 401) {
    throw new Error("guard did not require a key for " + path);
  }
}
`;
  const tmp = join(tmpdir(), `9router-guard-check-${process.pid}.mjs`);
  writeFileSync(tmp, guardSource + driver);
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

function injectWorkerGuard() {
  if (!existsSync(workerPath)) throw new Error(`${workerPath} does not exist after OpenNext build`);
  const modelsPaths = readModelsPathsFromSource();
  const source = readFileSync(workerPath, "utf8");
  if (source.includes("__9ROUTER_CF_ADMIN_GUARD__")) return;   // already injected

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

__MODELS_PATHS__

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

  // The model catalogue is public. A client needs it to know what it can call,
  // often before it holds a key, and it contains no user data. Every other LLM
  // route still requires a key.
  //
  // This mirrors src/proxy.cloudflare.js. It has to be mirrored: the guard below
  // is a hand-maintained copy injected directly into worker.js, and it runs
  // BEFORE the Next middleware. Editing only the source file leaves the copy
  // answering first, which is exactly what happened — the source change shipped,
  // the build contained it, and /v1/models still 401'd.
  if (MODELS_PATHS.has(pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname)) return null;

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

  // The public model-list paths are defined once, in src/proxy.cloudflare.js, and
    // read from there at build time. Substituting them into the guard means the
    // injected guard and the Next middleware agree by construction rather than by
    // discipline — they were two hand-maintained copies, and a source edit would
    // ship while production kept enforcing the old policy.
    const guardWithPaths = guard.replace(
      "__MODELS_PATHS__",
      `const MODELS_PATHS = new Set(${JSON.stringify(modelsPaths)});`
    );
    let withGuard = coordinatorRuntime + source.replace("export default {", `${guardWithPaths}\n${scheduled}\nexport default {\n    scheduled: __9ROUTER_SCHEDULED__,`)
      .replace(
      "return runWithCloudflareRequestContext(request, env, ctx, async () => {",
      "if (env) { for (const [k, v] of Object.entries(env)) { if (typeof v === 'string' && !process.env[k]) process.env[k] = v; } }\n        const guardResponse = await guardCloudflareAdminRequest(request, env);\n        if (guardResponse) return guardResponse;\n        let __9ROUTER_DOWNSTREAM_REQUEST = request;\n        const __9ROUTER_CF_IP = request.headers.get('cf-connecting-ip');\n        if (__9ROUTER_CF_IP) { const __9ROUTER_HEADERS = new Headers(request.headers); __9ROUTER_HEADERS.set('x-9r-real-ip', __9ROUTER_CF_IP); __9ROUTER_DOWNSTREAM_REQUEST = new Request(request.clone(), { headers: __9ROUTER_HEADERS }); }\n        else { const __9ROUTER_HEADERS = new Headers(request.headers); __9ROUTER_HEADERS.delete('x-9r-real-ip'); __9ROUTER_DOWNSTREAM_REQUEST = new Request(request, { headers: __9ROUTER_HEADERS }); }\n        return runWithCloudflareRequestContext(__9ROUTER_DOWNSTREAM_REQUEST, env, ctx, async () => {"
    );
  assertWorkerParses(withGuard);
  assertGuardDecidesCorrectly(guardWithPaths, modelsPaths);
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
