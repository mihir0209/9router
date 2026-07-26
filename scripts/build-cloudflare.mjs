import { renameSync, existsSync, readFileSync, writeFileSync } from "node:fs";
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

async function guardCloudflareAdminRequest(request, env) {
  const url = new URL(request.url);
  const { pathname } = url;

  if (isPublicCloudflarePath(pathname)) return null;

  if (isPublicLlmApi(pathname)) {
    if (hasApiKeyHeader(request, url)) return null;
    return Response.json({ error: "API key required" }, { status: 401 });
  }

  if (pathname.startsWith("/api/") || pathname.startsWith("/dashboard")) {
    if (await hasValidDashboardCookie(request, env)) return null;
    if (pathname.startsWith("/dashboard")) return Response.redirect(new URL("/login", request.url), 302);
    return Response.json({ error: "Unauthorized" }, { status: 401 });
  }

  return null;
}
`;

  const withGuard = source.replace("export default {", `${guard}\nexport default {`)
    .replace(
      "return runWithCloudflareRequestContext(request, env, ctx, async () => {",
      "const guardResponse = await guardCloudflareAdminRequest(request, env);\n        if (guardResponse) return guardResponse;\n        return runWithCloudflareRequestContext(request, env, ctx, async () => {"
    );
  writeFileSync(workerPath, withGuard);
}

try {
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
