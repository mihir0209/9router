const MAX_AGE_MS = 30_000;

function secret() {
  return process.env.INTERNAL_API_TOKEN || process.env.JWT_SECRET || process.env.ADMIN_API_TOKEN || process.env.API_KEY_SECRET || "";
}

function bytes(value) {
  return new TextEncoder().encode(value);
}

function toHex(buffer) {
  return [...new Uint8Array(buffer)].map((value) => value.toString(16).padStart(2, "0")).join("");
}

async function sign(timestamp, pathname) {
  const key = secret();
  if (!key) return "";
  const cryptoKey = await crypto.subtle.importKey("raw", bytes(key), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return toHex(await crypto.subtle.sign("HMAC", cryptoKey, bytes(`${timestamp}:${pathname}`)));
}

export async function createInternalRequestHeaders(pathname) {
  const timestamp = String(Date.now());
  return {
    "x-9r-dashboard-internal": "1",
    "x-9r-internal-timestamp": timestamp,
    "x-9r-internal-signature": await sign(timestamp, pathname),
  };
}

export async function isTrustedInternalRequest(request) {
  if (request.headers.get("x-9r-dashboard-internal") !== "1") return false;
  const timestamp = Number(request.headers.get("x-9r-internal-timestamp"));
  const supplied = request.headers.get("x-9r-internal-signature") || "";
  if (!Number.isFinite(timestamp) || Math.abs(Date.now() - timestamp) > MAX_AGE_MS || !supplied) return false;
  const pathname = new URL(request.url).pathname;
  const expected = await sign(String(timestamp), pathname);
  if (!expected || supplied.length !== expected.length) return false;
  let diff = 0;
  for (let index = 0; index < expected.length; index++) diff |= expected.charCodeAt(index) ^ supplied.charCodeAt(index);
  return diff === 0;
}
