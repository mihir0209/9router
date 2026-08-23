import { describe, it, expect } from "vitest";
import { getCapabilitiesForModel } from "../../open-sse/providers/capabilities.js";
import antigravityRegistry from "../../open-sse/providers/registry/antigravity.js";
import geminiRegistry from "../../open-sse/providers/registry/gemini.js";
import { MODEL_PRICING } from "../../open-sse/providers/pricing.js";
import { AntigravityExecutor } from "../../open-sse/executors/antigravity.js";

describe("Gemini 3.7 Flash Support & Config (#3286, #3281)", () => {
  it("registers gemini-3.7-flash tiered models in antigravity provider registry", () => {
    const agIds = antigravityRegistry.models.map(m => m.id);
    expect(agIds).toContain("gemini-3.7-flash-high");
    expect(agIds).toContain("gemini-3.7-flash-medium");
    expect(agIds).toContain("gemini-3.7-flash-low");
    expect(agIds).not.toContain("gemini-3.7-flash");
  });

  it("registers gemini-3.7-flash in gemini provider registry", () => {
    const geminiIds = geminiRegistry.models.map(m => m.id);
    expect(geminiIds).toContain("gemini-3.7-flash");
  });

  it("resolves capabilities correctly for gemini-3.7 models with official limits", () => {
    const caps = getCapabilitiesForModel("antigravity", "gemini-3.7-flash-high");
    expect(caps.vision).toBe(true);
    expect(caps.reasoning).toBe(true);
    expect(caps.thinkingFormat).toBe("gemini-level");
    expect(caps.contextWindow).toBe(1048576);
    expect(caps.maxOutput).toBe(65536);
  });

  it("defines pricing matching gemini-3.6-flash baseline", () => {
    expect(MODEL_PRICING["gemini-3.7-flash"]).toEqual(MODEL_PRICING["gemini-3.6-flash"]);
    expect(MODEL_PRICING["gemini-3.7-flash-high"]).toEqual(MODEL_PRICING["gemini-3.6-flash-high"]);
    expect(MODEL_PRICING["gemini-3.7-flash-medium"]).toEqual(MODEL_PRICING["gemini-3.6-flash-medium"]);
    expect(MODEL_PRICING["gemini-3.7-flash-low"]).toEqual(MODEL_PRICING["gemini-3.6-flash-low"]);
  });

  it("normalizes a public tier alias at dispatch time", () => {
    const executor = new AntigravityExecutor();
    const output = executor.transformRequest("gemini-3.7-flash-high", {
      model: "gemini-3.7-flash-high",
      request: {
        contents: [{ role: "user", parts: [{ text: "hi" }] }],
        generationConfig: { maxOutputTokens: 16 },
      },
    }, false, { projectId: "test-project", connectionId: "test-connection" });

    expect(output.model).toBe("gemini-3.7-flash-tiered");
    expect(output.request.generationConfig.thinkingConfig).toEqual({
      thinkingLevel: "high",
      includeThoughts: true,
    });
  });
});
