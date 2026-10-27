import { describe, it, expect } from "vitest";
import { validateBackup, previewMerge, mergeBackups } from "../../src/lib/db/mergeBackups.js";

/**
 * The client-side merge runs on a real backup before POSTing the result.
 * Two production bugs lived here:
 *
 *  1. customModels rows were keyed by row.id alone. Model ids collide across
 *     providers (a real 5k-model backup carried 798 shared ids), so merging
 *     collapsed distinct provider models into one row — silent data loss.
 *     The importer stores them under `${providerAlias}|${id}|${type}`; the
 *     merge must key the same way.
 *
 *  2. Row lookup used findIndex inside the incoming loop — O(n²). Merging a
 *     5k-row table into a 5k-row base is ~28M scans and froze the tab.
 */
describe("mergeBackups", () => {
  it("keeps custom models from different providers that share a model id", () => {
    const base = {
      customModels: [
        { providerAlias: "anthropic", id: "claude-opus-4.7", type: "llm", name: "Opus" },
      ],
    };
    const incoming = {
      customModels: [
        { providerAlias: "gemini", id: "claude-opus-4.7", type: "llm", name: "Opus via Gemini" },
      ],
    };

    const { merged, errors } = mergeBackups(base, incoming, "incoming-wins");
    expect(errors).toEqual([]);
    expect(merged.customModels).toHaveLength(2);
    expect(merged.customModels.map((m) => m.providerAlias).sort()).toEqual(["anthropic", "gemini"]);
  });

  it("updates the same custom model (same provider, same id) instead of duplicating it", () => {
    const base = {
      customModels: [{ providerAlias: "oc", id: "union-alpha", type: "llm", name: "old" }],
    };
    const incoming = {
      customModels: [{ providerAlias: "oc", id: "union-alpha", type: "llm", name: "new" }],
    };

    const { merged } = mergeBackups(base, incoming, "incoming-wins");
    expect(merged.customModels).toHaveLength(1);
    expect(merged.customModels[0].name).toBe("new");
  });

  it("re-merging an identical table is a no-op with every row unchanged", () => {
    const customModels = [
      { providerAlias: "oc", id: "union-alpha", type: "llm", name: "Union Alpha" },
      { providerAlias: "oc", id: "jev-1.13-free", type: "llm", name: "Jev" },
      { providerAlias: "art", id: "union-alpha", type: "image", name: "Union Alpha Image" },
    ];
    const backup = { customModels };

    const { merged, stats } = mergeBackups(backup, backup, "newer-wins");
    expect(merged.customModels).toHaveLength(3);
    expect(stats.added).toBe(0);
    expect(stats.updated).toBe(0);
    expect(stats.unchanged).toBe(3);
  });

  it("previewMerge counts colliding custom models as additions, not conflicts", () => {
    const base = { customModels: [{ providerAlias: "anthropic", id: "opus", type: "llm" }] };
    const incoming = {
      customModels: [
        { providerAlias: "anthropic", id: "opus", type: "llm" },        // unchanged
        { providerAlias: "gemini", id: "opus", type: "llm" },           // distinct → added
        { providerAlias: "anthropic", id: "opus", type: "image" },      // distinct kind → added
      ],
    };
    const stats = previewMerge(base, incoming, "newer-wins");
    expect(stats.added).toBe(2);
    expect(stats.unchanged).toBe(1);
    expect(stats.conflict).toBe(0);
  });

  it("merges a 5k-row table quickly (O(n) lookup, not findIndex)", () => {
    // Offset must be a multiple of the provider count so overlapping ids keep
    // their providerAlias — otherwise the composite key never matches and
    // everything counts as an addition.
    const big = (offset) => ({
      customModels: Array.from({ length: 5000 }, (_, i) => ({
        providerAlias: `prov-${i % 40}`,
        id: `model-${i + offset}`,
        type: "llm",
        name: `Model ${i + offset}`,
      })),
    });
    const base = big(0);
    const incoming = big(2520); // 2480 of 5000 incoming ids already exist in base

    const t0 = Date.now();
    const { merged, stats } = mergeBackups(base, incoming, "newer-wins");
    const ms = Date.now() - t0;

    expect(merged.customModels).toHaveLength(7520); // 5000 base + 2520 added
    expect(stats.added).toBe(2520);
    expect(stats.unchanged).toBe(2480);
    // The old findIndex version needed ~30s+ for this size; fail loudly if
    // complexity ever regresses. Generous bound to avoid CI flakiness.
    expect(ms).toBeLessThan(2000);
  });

  it("still keys id-based tables (connections, apiKeys, combos) by their row id", () => {
    const base = { apiKeys: [{ id: "k1", key: "sk-a", name: "A" }] };
    const incoming = {
      apiKeys: [
        { id: "k1", key: "sk-a", name: "A", updatedAt: "2026-01-02T00:00:00Z" },
        { id: "k2", key: "sk-b", name: "B" },
      ],
    };
    const { merged, stats } = mergeBackups(base, incoming, "newer-wins");
    expect(merged.apiKeys).toHaveLength(2);
    expect(stats.added).toBe(1);
    expect(stats.updated).toBe(1);
  });

  it("validateBackup accepts a full export shape and rejects null tables", () => {
    const full = {
      settings: {}, providerConnections: [], providerNodes: [], proxyPools: [],
      apiKeys: [], combos: [], modelAliases: {}, customModels: [], mitmAlias: {}, pricing: {},
    };
    expect(validateBackup(full).valid).toBe(true);
    expect(validateBackup({ settings: null }).valid).toBe(false);
    expect(validateBackup([]).valid).toBe(false);
  });
});
