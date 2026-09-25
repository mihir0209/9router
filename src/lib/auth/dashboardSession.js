import { SignJWT, jwtVerify } from "jose";
import { getSettings } from "@/lib/localDb";

const DEFAULT_PASSWORD = "123456";
const SESSION_MAX_AGE_SEC = 24 * 60 * 60;

function loadJwtSecret() {
  if (process.env.JWT_SECRET) return process.env.JWT_SECRET;
  if (process.env.NINEROUTER_RUNTIME === "cloudflare") {
    throw new Error("JWT_SECRET is required when running on Cloudflare Workers");
  }
  const fs = process.getBuiltinModule?.("node:fs");
  const path = process.getBuiltinModule?.("node:path");
  const os = process.getBuiltinModule?.("node:os");
  const dataDir = process.env.DATA_DIR || (os ? os.homedir() + "/.9router" : null);
  if (!fs || !path || !dataDir) return null;
  const file = path.join(dataDir, "jwt-secret");
  try {
    return fs.readFileSync(file, "utf8").trim();
  } catch {}
  fs.mkdirSync(dataDir, { recursive: true });
  const bytes = new Uint8Array(32);
  globalThis.crypto.getRandomValues(bytes);
  const generated = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  fs.writeFileSync(file, generated, { mode: 0o600 });
  return generated;
}

let secretBytes = null;

function getSecretBytes() {
  if (!secretBytes) secretBytes = new TextEncoder().encode(loadJwtSecret());
  return secretBytes;
}

export function shouldUseSecureCookie(request) {
  const forceSecureCookie = process.env.AUTH_COOKIE_SECURE === "true";
  const forwardedProto = request?.headers?.get?.("x-forwarded-proto");
  const isHttpsRequest = forwardedProto === "https";
  return forceSecureCookie || isHttpsRequest;
}

export async function createDashboardAuthToken(claims = {}) {
  return new SignJWT({ authenticated: true, ...claims })
    .setProtectedHeader({ alg: "HS256" })
    .setIssuedAt()
    .setExpirationTime("24h")
    .sign(getSecretBytes());
}

export async function verifyDashboardAuthToken(token) {
  if (!token) return false;
  try {
    await jwtVerify(token, getSecretBytes());
    return true;
  } catch {
    return false;
  }
}

export async function getDashboardAuthSession(token) {
  if (!token) return null;
  try {
    const { payload } = await jwtVerify(token, getSecretBytes());
    return payload;
  } catch {
    return null;
  }
}

export async function setDashboardAuthCookie(cookieStore, request, claims = {}) {
  const token = await createDashboardAuthToken(claims);
  cookieStore.set("auth_token", token, {
    httpOnly: true,
    secure: shouldUseSecureCookie(request),
    sameSite: "lax",
    path: "/",
    maxAge: SESSION_MAX_AGE_SEC,
  });
}

export function clearDashboardAuthCookie(cookieStore) {
  cookieStore.delete("auth_token");
}

// Verify the current dashboard password (re-auth for sensitive actions).
export async function verifyDashboardPassword(password) {
  if (typeof password !== "string" || !password) return false;
  const settings = await getSettings();
  const storedHash = settings?.password;
  if (storedHash) {
    const { default: bcrypt } = await import("bcryptjs");
    return bcrypt.compare(password, storedHash);
  }
  const initialPassword = process.env.INITIAL_PASSWORD || DEFAULT_PASSWORD;
  return password === initialPassword;
}
