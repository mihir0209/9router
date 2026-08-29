import { describe, it, expect } from "vitest";

const LIVE_KEY = process.env.PROXYRISE_LIVE_KEY || "";
const RUN_LIVE = process.env.PROXYRISE_LIVE_TEST === "1" && LIVE_KEY.startsWith("pgw-");

describe("ProxyRise live gateway (opt-in: PROXYRISE_LIVE_TEST=1 + PROXYRISE_LIVE_KEY)", () => {
  it.skipIf(!RUN_LIVE)("validates generated entries through the real pipeline", async () => {
    const { getProvider } = await import("@/lib/proxy/providers/registry.js");
    const { validateProxyList } = await import("@/lib/proxy/validateProxies.js");

    const provider = await getProvider("proxyrise");

    const countries = ["VG", "US"];
    const entries = [];
    for (const country of countries) {
      const batch = await provider.fetch({ apiKey: LIVE_KEY, country, perCountry: 1 });
      entries.push(...batch);
    }
    expect(entries).toHaveLength(countries.length);

    const summary = await validateProxyList(entries, {
      timeoutMs: 15000,
      validationConcurrency: 2,
      minWorkingProxies: 1,
    });

    const workingCountries = summary.working
      .map((p) => `${p.username.split("@")[0]}`)
      .join(", ");
    console.log(
      `[proxyrise-live] checked=${summary.checkedCount} working=${summary.workingCount} failed=${summary.failedCount} working_entries=${workingCountries}`
    );

    expect(summary.checkedCount).toBe(entries.length);
    expect(summary.workingCount).toBeGreaterThanOrEqual(1);
  }, 60000);
});
