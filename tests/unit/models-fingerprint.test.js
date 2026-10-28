import { describe, expect, it, vi } from "vitest";

vi.mock("@/lib/localDb", () => ({
  getProviderConnections: vi.fn(async () => []),
  getCombos: vi.fn(async () => []),
  getCustomModels: vi.fn(async () => []),
  getModelAliases: vi.fn(async () => ({})),
  getApiKeyByKey: vi.fn(async () => null),
  getSettings: vi.fn(async () => ({})),
}));
vi.mock("@/lib/disabledModelsDb", () => ({ getDisabledModels: vi.fn(async () => ({})) }));

const base = () => ({
  connections: [{ id: "c1", provider: "openai", isActive: true, priority: 1 }],
  combos: [{ name: "auto", models: ["openai/gpt-4o"], kind: null }],
  customModels: [{ providerAlias: "openai", id: "x", name: "X", kind: "llm" }],
  modelAliases: { a: "openai/gpt-4o" },
  disabledByAlias: {},
});

describe("modelsInputsEtag", () => {
  it("is stable for the same inputs, including repeated calls on the same objects", async () => {
    const { modelsInputsEtag } = await import("@/app/api/v1/models/route.js");
    const inputs = base();
    const first = modelsInputsEtag(inputs, "x");
    expect(modelsInputsEtag(inputs, "x")).toBe(first);
    expect(modelsInputsEtag(base(), "x")).toBe(first);
  });

  it("moves when a custom model or alias is replaced (the repos swap objects on write)", async () => {
    const { modelsInputsEtag } = await import("@/app/api/v1/models/route.js");
    const inputs = base();
    const before = modelsInputsEtag(inputs);
    const withModel = { ...inputs, customModels: [...inputs.customModels, { providerAlias: "openai", id: "y", name: "Y", kind: "llm" }] };
    expect(modelsInputsEtag(withModel)).not.toBe(before);
    const withAlias = { ...inputs, modelAliases: { ...inputs.modelAliases, b: "openai/gpt-4o-mini" } };
    expect(modelsInputsEtag(withAlias)).not.toBe(before);
    // the original objects still hash to the original value
    expect(modelsInputsEtag(inputs)).toBe(before);
  });

  it("moves when small inputs change", async () => {
    const { modelsInputsEtag } = await import("@/app/api/v1/models/route.js");
    const inputs = base();
    const before = modelsInputsEtag(inputs);
    expect(modelsInputsEtag({ ...inputs, disabledByAlias: { openai: ["gpt-4o"] } })).not.toBe(before);
    expect(modelsInputsEtag({ ...inputs, combos: [] })).not.toBe(before);
    expect(modelsInputsEtag({ ...inputs, connections: [] })).not.toBe(before);
  });
});
