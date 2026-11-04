import { describe, it, expect } from "vitest";
import { resolveProviderAlias, parseModel } from "../../open-sse/services/model.js";
import { getDefaultModel, getProviderModels } from "../../open-sse/config/providerModels.js";
import { resolveProviderId } from "../../src/shared/constants/providers.js";

describe("OpenRouter provider resolution and models", () => {
  it("resolves 'or' alias to 'openrouter'", () => {
    expect(resolveProviderAlias("or")).toBe("openrouter");
    expect(resolveProviderId("or")).toBe("openrouter");
    expect(resolveProviderAlias("openrouter")).toBe("openrouter");
    expect(resolveProviderId("openrouter")).toBe("openrouter");
  });

  it("parses or/model correctly as provider openrouter", () => {
    const parsed = parseModel("or/liquid/lfm-2.5-2.6b:free");
    expect(parsed.provider).toBe("openrouter");
    expect(parsed.model).toBe("liquid/lfm-2.5-2.6b:free");
  });

  it("getDefaultModel for openrouter returns an LLM chat model, not an embedding model", () => {
    const defaultModel = getDefaultModel("openrouter");
    expect(defaultModel).toBeTruthy();
    expect(defaultModel).not.toContain("embedding");
    const models = getProviderModels("openrouter");
    const matched = models.find((m) => m.id === defaultModel);
    expect(!matched.kind || matched.kind === "llm").toBe(true);
  });

  it("forces scoped resync from adapter when a requested provider is missing in cache", async () => {
    const { getProviderConnections, resetConnectionsCache } = await import("../../src/lib/db/repos/connectionsRepo.js");
    resetConnectionsCache();

    const conns = await getProviderConnections({ provider: "non-existent-provider-xyz" });
    expect(conns).toEqual([]);
  });
});
