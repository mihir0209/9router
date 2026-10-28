import { describe, it, expect, beforeAll, afterAll, vi } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// load() runs several times per request. The mtime stat that detects out-of-band edits is rate
// limited to one per second (it was ~3.7% of server CPU); invalidateCatalog() (called by the sync)
// must still force an immediate re-read.
let dir;
let mod;
let statSpy;

beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), "catalog-throttle-"));
  process.env.DATA_DIR = dir;
  vi.resetModules();
  mod = await import("../../open-sse/providers/catalogOverride.js");
  statSpy = vi.spyOn(fs, "statSync");
});

afterAll(() => {
  vi.restoreAllMocks();
  fs.rmSync(dir, { recursive: true, force: true });
});

const write = (models) => fs.writeFileSync(mod.CATALOG_FILE, JSON.stringify({ v: mod.CATALOG_VERSION, models, providers: {}, pricing: {} }));

describe("catalogOverride stat throttle", () => {
  it("does not stat the catalog on every lookup (including when the file is absent)", () => {
    mod.invalidateCatalog();
    statSpy.mockClear();
    for (let i = 0; i < 200; i++) mod.getCatalogModalities("p", "m");
    expect(statSpy.mock.calls.filter(([f]) => f === mod.CATALOG_FILE).length).toBeLessThanOrEqual(1);
  });

  it("invalidateCatalog forces an immediate re-read of a changed file", () => {
    write({ "p:m": { vision: true } });
    mod.invalidateCatalog();
    expect(mod.getCatalogModalities("p", "m")).toEqual({ vision: true });
    write({ "p:m": { vision: false } });
    expect(mod.getCatalogModalities("p", "m")).toEqual({ vision: true }); // still within the throttle window
    mod.invalidateCatalog();
    expect(mod.getCatalogModalities("p", "m")).toEqual({ vision: false });
  });

  it("notices an external edit once the interval has elapsed", () => {
    vi.useFakeTimers();
    try {
      write({ "p:m": { vision: true } });
      mod.invalidateCatalog();
      expect(mod.getCatalogModalities("p", "m")).toEqual({ vision: true });
      // mtime must differ for the reload to trigger
      write({ "p:m": { vision: false } });
      const future = new Date(Date.now() + 5000);
      fs.utimesSync(mod.CATALOG_FILE, future, future);
      vi.advanceTimersByTime(1100);
      expect(mod.getCatalogModalities("p", "m")).toEqual({ vision: false });
    } finally {
      vi.useRealTimers();
    }
  });
});
