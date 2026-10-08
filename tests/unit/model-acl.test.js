import { describe, it, expect } from "vitest";
import { isModelAllowed, filterAllowedModels } from "@/sse/services/modelAcl.js";

describe("modelAcl.isModelAllowed (upstream #4538, adapted)", () => {
  it("treats empty allowlist and '*' as unrestricted", () => {
    expect(isModelAllowed("", "anything")).toBe(true);
    expect(isModelAllowed([], "anything")).toBe(true);
    expect(isModelAllowed("*", "anything")).toBe(true);
    expect(isModelAllowed(["*"], "openai/gpt-4o")).toBe(true);
  });

  it("matches exact ids, slugs, providers and pairs", () => {
    expect(isModelAllowed("openai/gpt-4o", "openai/gpt-4o")).toBe(true);
    expect(isModelAllowed("gpt-4o", "openai/gpt-4o", { provider: "openai", model: "gpt-4o" })).toBe(true);
    expect(isModelAllowed("openai", "openai/gpt-4o")).toBe(true);
    expect(isModelAllowed("openai/gpt-4o", "anthropic/claude-opus")).toBe(false);
  });

  it("matches wildcards", () => {
    expect(isModelAllowed("openai/*", "openai/gpt-4o")).toBe(true);
    expect(isModelAllowed("openai/*", "anthropic/claude-opus")).toBe(false);
    expect(isModelAllowed("*gpt*", "openai/gpt-4o")).toBe(true);
    expect(isModelAllowed("deepseek*", "deepseek-chat")).toBe(true);
  });

  it("matches family prefixes with a boundary, not mid-word", () => {
    expect(isModelAllowed("deepseek", "deepseek-chat")).toBe(true);
    expect(isModelAllowed("qwen", "qwen/qwen-plus")).toBe(true);
    expect(isModelAllowed("deepseek", "deepseekx-chat")).toBe(false);
    expect(isModelAllowed("gpt", "gpt-4o")).toBe(true);
  });

  it("rejects empty requests and is case-insensitive", () => {
    expect(isModelAllowed("openai/gpt-4o", "")).toBe(false);
    expect(isModelAllowed("OpenAI/GPT-4o", "openai/gpt-4o")).toBe(true);
    expect(isModelAllowed("DEEPSEEK", "deepseek-chat")).toBe(true);
  });

  it("accepts JSON-encoded allowlists", () => {
    expect(isModelAllowed('["openai/*"]', "openai/gpt-4o")).toBe(true);
    expect(isModelAllowed("openai/gpt-4o, anthropic/*", "anthropic/claude-opus")).toBe(true);
  });
});

describe("modelAcl.filterAllowedModels", () => {
  const catalog = [
    { id: "openai/gpt-4o" },
    { id: "anthropic/claude-opus" },
    { id: "deepseek-chat" },
  ];
  it("passes everything through when unrestricted", () => {
    expect(filterAllowedModels(catalog, "")).toBe(catalog);
    expect(filterAllowedModels(catalog, "*")).toBe(catalog);
  });
  it("filters by wildcard and family", () => {
    expect(filterAllowedModels(catalog, "openai/*").map((m) => m.id)).toEqual(["openai/gpt-4o"]);
    expect(filterAllowedModels(catalog, "deepseek").map((m) => m.id)).toEqual(["deepseek-chat"]);
  });
  it("returns [] for non-lists", () => {
    expect(filterAllowedModels(null, "openai/*")).toEqual([]);
  });
});
