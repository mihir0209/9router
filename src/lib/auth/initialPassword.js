// Example INITIAL_PASSWORD values from .env.example / README / gitbook. Copied verbatim
// they are as public as the built-in default, so they must not unlock remote login.
// tests/unit/login-placeholder-initial-password.test.js fails if a doc adds a new one.
const PLACEHOLDER_INITIAL_PASSWORDS = new Set([
  "123456",
  "change-me",
  "your-password",
  "your-secure-password",
  "votre-mot-de-passe",
  "tu-contraseña",
]);

export function isPlaceholderInitialPassword(value) {
  const trimmed = value == null ? "" : String(value).trim();
  return !trimmed || PLACEHOLDER_INITIAL_PASSWORDS.has(trimmed);
}

/**
 * Resolve INITIAL_PASSWORD from Node process.env or Cloudflare Worker env context.
 */
export async function resolveInitialPassword() {
  const fromEnv = typeof process !== "undefined" ? process.env?.INITIAL_PASSWORD : undefined;
  if (fromEnv && String(fromEnv).trim()) {
    return String(fromEnv).trim();
  }
  try {
    const { getCloudflareContext } = await import("@opennextjs/cloudflare");
    const ctx = await getCloudflareContext({ async: true });
    const fromCf = ctx?.env?.INITIAL_PASSWORD;
    if (fromCf && String(fromCf).trim()) {
      return String(fromCf).trim();
    }
  } catch {
    // Not running under OpenNext / Cloudflare Workers
  }
  return "";
}

// True only when the operator set INITIAL_PASSWORD to something of their own.
export function hasOperatorInitialPassword(overrideValue) {
  const value = overrideValue !== undefined
    ? overrideValue
    : (typeof process !== "undefined" ? process.env?.INITIAL_PASSWORD : undefined);
  return Boolean(value && String(value).trim()) && !isPlaceholderInitialPassword(value);
}
