/**
 * The dashboard JWT secret has to be identical in two runtimes:
 *
 *  - the Node runtime signs and verifies the session token in route handlers;
 *  - the Edge runtime (the proxy/middleware) verifies it too, and the Edge
 *    sandbox has no filesystem — it can only read process.env.
 *
 * So when the secret lives in a file (the default: <dataDir>/jwt-secret), the
 * Edge side silently verifies against a different key and every authenticated
 * request bounces back to /login. Publishing the file's secret into the
 * environment at boot gives both sides the same key.
 *
 * `process.getBuiltinModule` is used instead of a static "node:fs" import so
 * this module can be bundled for the Edge runtime, where it simply reports
 * "no filesystem" and does nothing.
 */
export function ensureJwtSecretEnv() {
  if (process.env.JWT_SECRET) return process.env.JWT_SECRET;

  const fs = process.getBuiltinModule?.("node:fs");
  const path = process.getBuiltinModule?.("node:path");
  const os = process.getBuiltinModule?.("node:os");
  if (!fs || !path) return null;

  const dataDir = process.env.DATA_DIR || (os ? os.homedir() + "/.9router" : null);
  if (!dataDir) return null;

  const file = path.join(dataDir, "jwt-secret");
  try {
    const existing = fs.readFileSync(file, "utf8").trim();
    if (existing) {
      process.env.JWT_SECRET = existing;
      return existing;
    }
  } catch {
    /* not created yet */
  }

  try {
    fs.mkdirSync(dataDir, { recursive: true });
    const bytes = new Uint8Array(32);
    globalThis.crypto.getRandomValues(bytes);
    const generated = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
    fs.writeFileSync(file, generated, { mode: 0o600 });
    process.env.JWT_SECRET = generated;
    return generated;
  } catch {
    return null;
  }
}
