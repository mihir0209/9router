import { describe, it, expect } from "vitest";
import { augmentModelsWithCapacityAdapter } from "../../open-sse/services/capacityAdapter.js";
import { createComboMemberResolver } from "../../open-sse/services/combo.js";

const VISION = "anthropic/claude-sonnet-4.6";
const TEXT = "deepseek/deepseek-chat";
const POOL_VISION = "anthropic/claude-opus-5";

const settings = {
  capacityAdapter: {
    vision: { enabled: true, roundRobin: false, models: [POOL_VISION] },
  },
};

const combos = [
  { name: "pack-a", models: [TEXT, VISION] },
  { name: "pack-b", models: [TEXT] },
];
const resolve = createComboMemberResolver(combos);

describe("augmentModelsWithCapacityAdapter with nested combos", () => {
  it("does not prepend the pool when a nested combo covers vision", () => {
    const models = [TEXT, "pack-a"];
    const out = augmentModelsWithCapacityAdapter(models, new Set(["vision"]), settings, resolve);
    expect(out).toBe(models);
  });

  it("prepends the pool when nested combos lack vision (resolver on)", () => {
    const models = [TEXT, "pack-b"];
    const out = augmentModelsWithCapacityAdapter(models, new Set(["vision"]), settings, resolve);
    expect(out[0]).toBe(POOL_VISION);
    expect(out.slice(1)).toEqual(models);
  });

  it("old behavior without resolver: prepends even though the nested combo has vision", () => {
    const models = [TEXT, "pack-a"];
    const out = augmentModelsWithCapacityAdapter(models, new Set(["vision"]), settings);
    expect(out[0]).toBe(POOL_VISION);
  });

  it("no required capabilities -> unchanged", () => {
    const models = [TEXT, "vision-pack"];
    expect(augmentModelsWithCapacityAdapter(models, new Set(), settings, resolve)).toBe(models);
  });

  it("terminates on cyclic nested combos", () => {
    const cyclic = createComboMemberResolver([
      { name: "loop", models: ["loop", TEXT] },
    ]);
    const out = augmentModelsWithCapacityAdapter(["loop"], new Set(["vision"]), settings, cyclic);
    expect(out[0]).toBe(POOL_VISION);
    expect(out).toContain("loop");
  });
});
