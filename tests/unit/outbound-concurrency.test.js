import { describe, it, expect, afterEach } from "vitest";
import { capOutboundConcurrency, mapLimited, CLOUDFLARE_OUTBOUND_CONCURRENCY } from "../../src/lib/async/outboundConcurrency.js";

const saved = { r: process.env.NINEROUTER_RUNTIME, o: process.env.NINEROUTER_OUTBOUND_CONCURRENCY };
afterEach(() => {
  for (const [k, v] of [["NINEROUTER_RUNTIME", saved.r], ["NINEROUTER_OUTBOUND_CONCURRENCY", saved.o]]) {
    if (v === undefined) delete process.env[k]; else process.env[k] = v;
  }
});

describe("capOutboundConcurrency", () => {
  it("leaves the configured value alone outside Cloudflare", () => {
    delete process.env.NINEROUTER_RUNTIME;
    expect(capOutboundConcurrency(50)).toBe(50);
    expect(capOutboundConcurrency(10)).toBe(10);
  });
  it("caps at 5 on Cloudflare, never raises, and honours the override", () => {
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    expect(CLOUDFLARE_OUTBOUND_CONCURRENCY).toBe(5);
    expect(capOutboundConcurrency(50)).toBe(5);
    expect(capOutboundConcurrency(3)).toBe(3);
    expect(capOutboundConcurrency(0)).toBe(1);
    process.env.NINEROUTER_OUTBOUND_CONCURRENCY = "2";
    expect(capOutboundConcurrency(50)).toBe(2);
  });
});

describe("mapLimited", () => {
  it("keeps order, never exceeds the limit, and matches Promise.all results", async () => {
    let inFlight = 0, peak = 0;
    const items = Array.from({ length: 23 }, (_, i) => i);
    const out = await mapLimited(items, async (n) => {
      inFlight++; peak = Math.max(peak, inFlight);
      await new Promise((r) => setTimeout(r, (23 - n) % 4));
      inFlight--;
      return n * 2;
    }, 5);
    expect(out).toEqual(items.map((n) => n * 2));
    expect(peak).toBeLessThanOrEqual(5);
    expect(peak).toBeGreaterThan(1);
  });
  it("rejects like Promise.all (no fallback value)", async () => {
    await expect(mapLimited([1, 2, 3], async (n) => { if (n === 2) throw new Error("boom"); return n; }, 2)).rejects.toThrow("boom");
  });
  it("handles an empty list", async () => {
    expect(await mapLimited([], async () => 1, 5)).toEqual([]);
  });
});
