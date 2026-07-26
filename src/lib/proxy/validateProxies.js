import { testProxyUrl } from "@/lib/network/proxyTest";

const FETCH_TIMEOUT_MS = 30000;

export { FETCH_TIMEOUT_MS };

/**
 * Validate a list of proxies by testing each one.
 * @param {Array<{proxyUrl: string}>} proxies - Proxies to test
 * @param {object} config - Validation config
 * @param {number} config.timeoutMs - Test timeout per proxy
 * @param {number} config.validationConcurrency - Max parallel tests
 * @param {number} config.minWorkingProxies - Minimum working count required
 * @returns {{ checkedCount, workingCount, failedCount, working, results }}
 */
export async function validateProxyList(proxies, config) {
  const results = new Array(proxies.length);
  const queue = proxies.map((proxy, index) => ({ proxy, index }));

  const worker = async () => {
    while (queue.length > 0) {
      const item = queue.shift();
      if (!item) break;
      const validation = await testProxyUrl({
        proxyUrl: item.proxy.proxyUrl,
        testUrl: "https://api.ipify.org/",
        method: "GET",
        timeoutMs: Math.min(config.timeoutMs || 10000, 15000),
      });
      results[item.index] = {
        ...item.proxy,
        validation,
      };
    }
  };

  await Promise.all(
    Array.from(
      { length: Math.min(config.validationConcurrency || 10, proxies.length) },
      worker
    )
  );

  const working = results.filter((entry) => entry?.validation?.ok);
  return {
    checkedCount: results.filter(Boolean).length,
    workingCount: working.length,
    failedCount: results.filter((entry) => entry && !entry.validation?.ok).length,
    working,
    results,
  };
}

/**
 * Parse a text proxy list (one proxy per line, ip:port or protocol://ip:port).
 */
export function parseTextProxyList(text, defaultProtocol = "http") {
  const seen = new Set();
  const proxies = [];

  for (const line of String(text || "").split(/\r?\n/)) {
    const raw = line.trim();
    if (!raw || raw.startsWith("#")) continue;

    const proxyUrl = raw.includes("://") ? raw : `${defaultProtocol}://${raw}`;

    try {
      const parsed = new URL(proxyUrl);
      if (!parsed.hostname || !parsed.port) continue;
      const normalized = parsed.toString();
      if (seen.has(normalized)) continue;
      seen.add(normalized);
      proxies.push({
        proxyUrl: normalized,
        protocol: parsed.protocol.replace(":", ""),
        host: parsed.hostname,
        port: parsed.port,
        hostLabel: `${parsed.hostname}:${parsed.port}`,
      });
    } catch {
      // Skip malformed lines
    }
  }

  return proxies;
}
