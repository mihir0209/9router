import { isIP } from "node:net";

function normalizeIp(value) {
  return String(value || "").trim().replace(/^\[|\]$/g, "").toLowerCase();
}

function parseAddress(value) {
  const normalized = normalizeIp(value);
  const version = isIP(normalized);
  if (!version) return null;
  return { version, value: version === 4 ? ipv4BigInt(normalized) : ipv6BigInt(normalized) };
}

function ipv4BigInt(value) {
  return value.split(".").reduce((total, part) => (total << 8n) + BigInt(Number(part)), 0n);
}

function ipv6BigInt(value) {
  const [left, right] = value.split("::");
  const leftParts = left ? left.split(":") : [];
  const rightParts = right ? right.split(":") : [];
  const missing = 8 - leftParts.length - rightParts.length;
  const parts = [...leftParts, ...Array.from({ length: Math.max(missing, 0) }, () => "0"), ...rightParts];
  return parts.reduce((total, part) => (total << 16n) + BigInt.parseInt(part || "0", 16), 0n);
}

function prefixLength(value, version) {
  const maximum = version === 4 ? 32 : 128;
  if (!Number.isInteger(value) || value < 0 || value > maximum) return null;
  return value;
}

export function isValidIpRule(value) {
  const rule = String(value || "").trim();
  if (!rule) return false;
  if (!rule.includes("/")) return isIP(normalizeIp(rule)) !== 0;
  const [address, bits] = rule.split("/");
  const parsed = parseAddress(address);
  if (!parsed || !/^\d+$/.test(bits)) return false;
  return prefixLength(Number(bits), parsed.version) !== null;
}

export function ipMatchesRule(ip, rule) {
  const client = parseAddress(ip);
  const normalizedRule = String(rule || "").trim();
  if (!client) return false;
  if (!normalizedRule.includes("/")) return normalizeIp(ip) === normalizeIp(normalizedRule);
  const [address, bitsText] = normalizedRule.split("/");
  const network = parseAddress(address);
  const bits = prefixLength(Number(bitsText), network?.version);
  if (!network || bits === null || network.version !== client.version) return false;
  const width = network.version === 4 ? 32 : 128;
  const shift = BigInt(width - bits);
  return (client.value >> shift) === (network.value >> shift);
}

export function ipMatchesAny(ip, rules) {
  return rules.some((rule) => ipMatchesRule(ip, rule));
}

export function trustedClientIp(request) {
  const peerToken = process.env.NINEROUTER_PEER_TOKEN;
  if (peerToken && request.headers.get("x-9r-peer-token") === peerToken) {
    return normalizeIp(request.headers.get("x-9r-real-ip"));
  }
  if (process.env.NINEROUTER_RUNTIME === "cloudflare") {
    return normalizeIp(request.headers.get("cf-connecting-ip") || request.headers.get("x-9r-real-ip"));
  }
  if (process.env.NODE_ENV === "test") {
    return normalizeIp(request.headers.get("x-9r-real-ip"));
  }
  return "";
}
