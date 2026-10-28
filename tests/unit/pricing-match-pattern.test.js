import { describe, it, expect } from "vitest";
import { matchPattern, getPricingForModel } from "../../open-sse/providers/pricing.js";

// matchPattern compiles each glob once and reuses the RegExp (it ran per request and showed up as
// ~16% of server CPU in a load profile). These tests pin the matching semantics so caching the
// compiled regex cannot change results, in particular across repeated calls with the same pattern.
describe("matchPattern (glob, case-insensitive, cached regex)", () => {
  it("matches wildcards and is anchored at both ends", () => {
    expect(matchPattern("glm-*", "glm-4.6")).toBe(true);
    expect(matchPattern("glm-*", "xglm-4.6")).toBe(false);
    expect(matchPattern("gpt-4", "gpt-4o")).toBe(false);
    expect(matchPattern("*-mini", "o4-mini")).toBe(true);
  });

  it("is case-insensitive", () => {
    expect(matchPattern("MiniMax-*", "minimax-m2.5")).toBe(true);
    expect(matchPattern("minimax-*", "MiniMax-M2.5")).toBe(true);
  });

  it("escapes regex metacharacters in the pattern", () => {
    expect(matchPattern("gpt-4.1", "gpt-4.1")).toBe(true);
    expect(matchPattern("gpt-4.1", "gpt-4x1")).toBe(false);
    expect(matchPattern("a+b(c)", "a+b(c)")).toBe(true);
  });

  it("returns the same answer on repeated calls (shared regex is stateless)", () => {
    for (let i = 0; i < 5; i++) {
      expect(matchPattern("grok-*", "grok-4")).toBe(true);
      expect(matchPattern("grok-*", "other")).toBe(false);
    }
  });

  it("still resolves pattern pricing and unknown models", () => {
    expect(getPricingForModel("some-provider", "grok-code-fast-99")).toMatchObject({ input: 0.5, output: 2 });
    expect(getPricingForModel("some-provider", "totally-unknown-model-xyz")).toBeNull();
  });
});
