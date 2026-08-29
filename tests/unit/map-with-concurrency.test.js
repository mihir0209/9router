import { describe, expect, it, vi } from "vitest";
import { mapWithConcurrency } from "@/lib/async/mapWithConcurrency.js";

const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

describe("mapWithConcurrency", () => {
  it("executes work in parallel up to the configured cap", async () => {
    let active = 0;
    let peak = 0;

    const results = await mapWithConcurrency([30, 30, 30, 30, 30], async (ms) => {
      active += 1;
      peak = Math.max(peak, active);
      await delay(ms);
      active -= 1;
      return ms;
    }, { concurrency: 2, timeoutMs: 200 });

    expect(peak).toBe(2);
    expect(results).toEqual([30, 30, 30, 30, 30]);
  });

  it("isolates failures and preserves deterministic input order", async () => {
    const onError = vi.fn(() => "fallback");
    const results = await mapWithConcurrency([40, 5, 20], async (ms, index) => {
      await delay(ms);
      if (index === 1) throw new Error("broken provider");
      return `provider-${index}`;
    }, { concurrency: 3, timeoutMs: 100, onError });

    expect(results).toEqual(["provider-0", "fallback", "provider-2"]);
    expect(onError).toHaveBeenCalledOnce();
  });

  it("returns the fallback when a task exceeds its deadline", async () => {
    const startedAt = Date.now();
    let aborted = false;
    const results = await mapWithConcurrency([1], (_item, _index, signal) => new Promise(() => {
      signal.addEventListener("abort", () => { aborted = true; });
    }), {
      concurrency: 1,
      timeoutMs: 20,
      onError: (error) => error.name,
    });

    expect(results).toEqual(["TimeoutError"]);
    expect(aborted).toBe(true);
    expect(Date.now() - startedAt).toBeLessThan(150);
  });
});
