import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  getProviderConnections: vi.fn(),
  getCombos: vi.fn(async () => []),
  getCustomModels: vi.fn(async () => []),
  getModelAliases: vi.fn(async () => ({})),
  getDisabledModels: vi.fn(async () => ({})),
  resolveKiroModels: vi.fn(),
  resolveQoderModels: vi.fn(),
  resolveKimchiModels: vi.fn(),
}));

vi.mock("@/lib/localDb", () => ({
  getProviderConnections: mocks.getProviderConnections,
  getCombos: mocks.getCombos,
  getCustomModels: mocks.getCustomModels,
  getModelAliases: mocks.getModelAliases,
}));
vi.mock("@/lib/disabledModelsDb", () => ({ getDisabledModels: mocks.getDisabledModels }));
vi.mock("open-sse/services/kiroModels.js", () => ({ resolveKiroModels: mocks.resolveKiroModels }));
vi.mock("open-sse/services/qoderModels.js", () => ({
  resolveQoderModels: mocks.resolveQoderModels,
  // Route uses this to expand the live catalog; keep in sync with qoderModels.js shape.
  routableQoderModels: (catalog) => {
    if (!catalog) return [];
    return (catalog.models || []).map((m) => ({
      id: m.id,
      name: m.name || m.id,
      hidden: false,
    }));
  },
}));
vi.mock("open-sse/services/kimchiModels.js", () => ({ resolveKimchiModels: mocks.resolveKimchiModels }));
vi.mock("open-sse/services/copilotModels.js", () => ({ resolveCopilotModels: vi.fn() }));
vi.mock("open-sse/services/clinepassModels.js", () => ({ resolveClinepassModels: vi.fn() }));
vi.mock("open-sse/services/grokCliModels.js", () => ({ resolveGrokCliModels: vi.fn() }));
vi.mock("open-sse/services/cursorModels.js", () => ({ resolveCursorModels: vi.fn() }));
vi.mock("open-sse/shared/zedAuth.js", () => ({ resolveZedModels: vi.fn() }));
vi.mock("@/sse/services/tokenRefresh", () => ({ updateProviderCredentials: vi.fn() }));
vi.mock("@/lib/network/connectionProxy", () => ({ resolveConnectionProxyConfig: vi.fn() }));

import { buildModelsList } from "@/app/api/v1/models/route.js";

const connections = ["kiro", "qoder", "kimchi"].map((provider, index) => ({
  id: index + 1,
  provider,
  isActive: true,
  accessToken: "token",
  providerSpecificData: {},
}));

describe("model aggregation", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.getProviderConnections.mockResolvedValue(connections);
    mocks.getCombos.mockResolvedValue([]);
    mocks.getCustomModels.mockResolvedValue([]);
    mocks.getModelAliases.mockResolvedValue({});
    mocks.getDisabledModels.mockResolvedValue({});
  });

  it("starts provider catalogs concurrently and preserves provider order", async () => {
    const releases = [];
    const resolver = (id) => vi.fn(() => new Promise((resolve) => {
      releases.push(() => resolve({ models: [{ id }] }));
    }));
    mocks.resolveKiroModels.mockImplementation(resolver("kiro-live"));
    mocks.resolveQoderModels.mockImplementation(resolver("qoder-live"));
    mocks.resolveKimchiModels.mockImplementation(resolver("kimchi-live"));

    const pending = buildModelsList(["llm"], { providerConcurrency: 3, providerTimeoutMs: 200 });
    await vi.waitFor(() => expect(releases).toHaveLength(3));
    releases.reverse().forEach((release) => release());

    const result = await pending;
    expect(result.map((model) => model.id)).toEqual([
      "kr/kiro-live",
      "qd/qoder-live",
      "kimchi/kimchi-live",
    ]);
  });

  it("isolates provider failures and deadlines", async () => {
    mocks.resolveKiroModels.mockResolvedValue({ models: [{ id: "healthy" }] });
    mocks.resolveQoderModels.mockRejectedValue(new Error("broken"));
    mocks.resolveKimchiModels.mockImplementation(() => new Promise(() => {}));

    const result = await buildModelsList(["llm"], {
      providerConcurrency: 3,
      providerTimeoutMs: 20,
    });

    expect(result.some((model) => model.id === "kr/healthy")).toBe(true);
    expect(result.some((model) => model.owned_by === "qd")).toBe(true);
    expect(result.some((model) => model.owned_by === "kimchi")).toBe(false);
  });

  it("exposes configured custom media models only under their media kind", async () => {
    mocks.getProviderConnections.mockResolvedValue([{
      id: 99,
      provider: "custom-media-demo",
      isActive: true,
      apiKey: "secret",
      providerSpecificData: {
        prefix: "art",
        mediaKind: "image",
        models: ["image-alpha", "image-beta"],
      },
    }]);

    const images = await buildModelsList(["image"]);
    const llms = await buildModelsList(["llm"]);

    expect(images.map((model) => model.id)).toEqual(["art/image-alpha", "art/image-beta"]);
    expect(llms).toEqual([]);
  });
});
