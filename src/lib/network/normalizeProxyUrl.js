/**
 * Normalize proxy input into a URL undici ProxyAgent accepts.
 *
 * Supported:
 *   http(s)://user:pass@host:port
 *   socks5://host:port
 *   user:pass@host:port
 *   host:port
 *   host:port:user:pass          ← provider paste format (password may contain ":")
 *   scheme://host:port:user:pass ← rare; treated as colon-format after scheme strip if URL fails
 *
 * @param {unknown} input
 * @param {{ defaultScheme?: string }} [opts]
 * @returns {{ ok: true, url: string, hostLabel: string } | { ok: false, error: string }}
 */
export function normalizeProxyUrl(input, opts = {}) {
  const defaultScheme = opts.defaultScheme || "http";
  const raw = input == null ? "" : String(input).trim();
  if (!raw) return { ok: false, error: "Proxy URL is required" };
  if (/[\n\r`$]/.test(raw)) return { ok: false, error: "Proxy URL contains invalid characters" };

  // Already a full URL with scheme
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(raw)) {
    try {
      const parsed = new URL(raw);
      if (!parsed.hostname) return { ok: false, error: "Proxy URL missing host" };
      return {
        ok: true,
        url: parsed.href,
        hostLabel: parsed.port ? `${parsed.hostname}:${parsed.port}` : parsed.hostname,
      };
    } catch {
      // fall through — may be malformed scheme://host:port:user:pass
    }
  }

  // user:pass@host:port (no scheme)
  if (raw.includes("@") && !raw.includes("://")) {
    try {
      const parsed = new URL(`${defaultScheme}://${raw}`);
      return {
        ok: true,
        url: parsed.href,
        hostLabel: parsed.port ? `${parsed.hostname}:${parsed.port}` : parsed.hostname,
      };
    } catch {
      return { ok: false, error: "Invalid user:pass@host:port proxy" };
    }
  }

  // host:port  OR  host:port:user:pass  (password may contain additional ":")
  const colonFmt = parseHostPortUserPass(raw);
  if (colonFmt) {
    const { host, port, username, password } = colonFmt;
    if (username != null) {
      const auth = `${encodeURIComponent(username)}:${encodeURIComponent(password)}`;
      const url = `${defaultScheme}://${auth}@${host}:${port}`;
      try {
        const parsed = new URL(url);
        return { ok: true, url: parsed.href, hostLabel: `${host}:${port}` };
      } catch {
        return { ok: false, error: "Invalid host:port:user:pass proxy" };
      }
    }
    // host:port only
    try {
      const parsed = new URL(`${defaultScheme}://${host}:${port}`);
      return { ok: true, url: parsed.href, hostLabel: `${host}:${port}` };
    } catch {
      return { ok: false, error: "Invalid host:port proxy" };
    }
  }

  return { ok: false, error: "Unsupported proxy format. Use URL or host:port:user:pass" };
}

/**
 * Parse host:port or host:port:user:pass.
 * Password is everything after the 3rd colon (so ":" inside password is kept).
 * @returns {{ host: string, port: string, username?: string, password?: string } | null}
 */
export function parseHostPortUserPass(raw) {
  const s = String(raw || "").trim();
  if (!s || s.includes("://") || s.includes("@")) return null;

  const c1 = s.indexOf(":");
  if (c1 <= 0) return null;
  const c2 = s.indexOf(":", c1 + 1);

  // host:port
  if (c2 < 0) {
    const host = s.slice(0, c1);
    const port = s.slice(c1 + 1);
    if (!host || !isValidPort(port)) return null;
    return { host, port };
  }

  // host:port:user:pass...
  const c3 = s.indexOf(":", c2 + 1);
  if (c3 < 0) return null; // host:port:something — incomplete auth form

  const host = s.slice(0, c1);
  const port = s.slice(c1 + 1, c2);
  const username = s.slice(c2 + 1, c3);
  const password = s.slice(c3 + 1);

  if (!host || !isValidPort(port) || !username || password === "") return null;
  return { host, port, username, password };
}

function isValidPort(port) {
  if (!/^\d{1,5}$/.test(port)) return false;
  const n = Number(port);
  return n >= 1 && n <= 65535;
}

/** Convenience: return URL string or null */
export function toProxyUrlString(input, opts) {
  const r = normalizeProxyUrl(input, opts);
  return r.ok ? r.url : null;
}
