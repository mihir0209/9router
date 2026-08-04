import { randomUUID, randomBytes } from "node:crypto";

/**
 * Resolve header value templates:
 * - {uuid} → full UUID with dashes
 * - {uuid-nodash} → UUID without dashes
 * - {timestamp} → Unix timestamp in milliseconds
 * - {timestamp-sec} → Unix timestamp in seconds
 * - {random-hex-8} → 8-character hex string
 * - {random-hex-16} → 16-character hex string
 * - {random-hex-32} → 32-character hex string
 * - {iso8601} → ISO 8601 timestamp
 * - Static strings are returned as-is
 */

const TEMPLATE_PATTERNS = {
  uuid: () => randomUUID(),
  "uuid-nodash": () => randomUUID().replace(/-/g, ""),
  timestamp: () => Date.now().toString(),
  "timestamp-sec": () => Math.floor(Date.now() / 1000).toString(),
  "random-hex-8": () => randomBytes(4).toString("hex"),
  "random-hex-16": () => randomBytes(8).toString("hex"),
  "random-hex-32": () => randomBytes(16).toString("hex"),
  iso8601: () => new Date().toISOString(),
};

/**
 * Resolve a single header value that may contain multiple template placeholders.
 * Examples:
 *   "00-{uuid-nodash}-{random-hex-16}-01" → "00-abc123...-def456...-01"
 *   "WorkBuddy/{version} CLI/{timestamp}" → "WorkBuddy/5.2.7 CLI/1722792000000"
 */
export function resolveHeaderValue(template) {
  if (typeof template !== "string") return template;

  let resolved = template;
  for (const [name, generator] of Object.entries(TEMPLATE_PATTERNS)) {
    const placeholder = `{${name}}`;
    if (resolved.includes(placeholder)) {
      // Generate once per placeholder occurrence
      while (resolved.includes(placeholder)) {
        resolved = resolved.replace(placeholder, generator());
      }
    }
  }
  return resolved;
}

/**
 * Resolve all header values in an object.
 * @param {Object} headers - Map of header names to template strings
 * @returns {Object} - Map of header names to resolved values
 */
export function resolveHeaderTemplates(headers) {
  if (!headers || typeof headers !== "object") return {};

  const resolved = {};
  for (const [key, value] of Object.entries(headers)) {
    resolved[key] = resolveHeaderValue(value);
  }
  return resolved;
}

/**
 * Apply header blacklist by removing matching keys (case-insensitive).
 * @param {Object} headers - Current headers
 * @param {string[]} blacklist - Array of header names to remove
 * @returns {Object} - Filtered headers
 */
export function applyHeaderBlacklist(headers, blacklist) {
  if (!blacklist || !Array.isArray(blacklist) || blacklist.length === 0) {
    return headers;
  }

  const lowerBlacklist = new Set(blacklist.map((h) => h.toLowerCase()));
  const filtered = {};
  for (const [key, value] of Object.entries(headers)) {
    if (!lowerBlacklist.has(key.toLowerCase())) {
      filtered[key] = value;
    }
  }
  return filtered;
}
