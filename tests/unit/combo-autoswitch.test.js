import { describe, it, expect } from "vitest";
import { detectRequiredCapabilities, reorderByCapabilities, createComboMemberResolver } from "../../open-sse/services/combo.js";

describe("detectRequiredCapabilities", () => {
  it("text-only -> empty", () => {
    const r = detectRequiredCapabilities({ messages: [{ role: "user", content: "hi" }] });
    expect(r.size).toBe(0);
  });

  it("openai image_url -> vision", () => {
    const r = detectRequiredCapabilities({ messages: [{ role: "user", content: [
      { type: "image_url", image_url: { url: "x" } },
    ] }] });
    expect(r.has("vision")).toBe(true);
  });

  it("openai file -> pdf", () => {
    const r = detectRequiredCapabilities({ messages: [{ role: "user", content: [
      { type: "file", file: { file_data: "data:application/pdf;base64,x" } },
    ] }] });
    expect(r.has("pdf")).toBe(true);
  });

  it("gemini inlineData image -> vision", () => {
    const r = detectRequiredCapabilities({ contents: [{ role: "user", parts: [
      { inlineData: { mimeType: "image/png", data: "x" } },
    ] }] });
    expect(r.has("vision")).toBe(true);
  });

  it("antigravity request.contents image -> vision", () => {
    const r = detectRequiredCapabilities({ request: { contents: [{ role: "user", parts: [
      { inlineData: { mimeType: "image/jpeg", data: "x" } },
    ] }] } });
    expect(r.has("vision")).toBe(true);
  });

  it("web_search tool -> search", () => {
    const r = detectRequiredCapabilities({ messages: [{ role: "user", content: "q" }], tools: [
      { type: "web_search" },
    ] });
    expect(r.has("search")).toBe(true);
  });

  it("responses input_image -> vision", () => {
    const r = detectRequiredCapabilities({ input: [{ role: "user", content: [
      { type: "input_image", image_url: "x" },
    ] }] });
    expect(r.has("vision")).toBe(true);
  });
});

describe("reorderByCapabilities", () => {
  it("no required -> unchanged", () => {
    const models = ["a/x", "b/y"];
    expect(reorderByCapabilities(models, new Set())).toBe(models);
  });

  it("floats vision-capable model to front, keeps fallback", () => {
    // deepseek-chat = no vision; claude-sonnet = vision
    const models = ["deepseek/deepseek-chat", "anthropic/claude-sonnet-4.6"];
    const out = reorderByCapabilities(models, new Set(["vision"]));
    expect(out[0]).toBe("anthropic/claude-sonnet-4.6");
    expect(out).toContain("deepseek/deepseek-chat"); // not dropped
    expect(out).toHaveLength(2);
  });

  it("keeps order when no model matches", () => {
    const models = ["deepseek/deepseek-chat", "deepseek/deepseek-reasoner"];
    const out = reorderByCapabilities(models, new Set(["vision"]));
    expect(out).toBe(models);
  });

  it("single model -> unchanged", () => {
    const models = ["a/x"];
    expect(reorderByCapabilities(models, new Set(["vision"]))).toBe(models);
  });
});

describe("reorderByCapabilities with nested combos", () => {
  const VISION = "anthropic/claude-sonnet-4.6";
  const TEXT = "deepseek/deepseek-chat";
  const combos = [
    { name: "pack-a", models: [TEXT, VISION] },
    { name: "pack-b", models: [TEXT] },
  ];
  const resolve = createComboMemberResolver(combos);

  it("floats a nested combo containing a vision model ahead of later members", () => {
    const models = [TEXT, "pack-a", "pack-b"];
    const out = reorderByCapabilities(models, new Set(["vision"]), resolve);
    expect(out[0]).toBe("pack-a");
    expect(out).toHaveLength(3);
  });

  it("keeps in-line order among entries that all satisfy the capability", () => {
    const models = ["pack-a", VISION];
    const out = reorderByCapabilities(models, new Set(["vision"]), resolve);
    expect(out).toEqual(models);
  });

  it("old behavior without resolver: nested combos score as unknown bare ids", () => {
    const models = ["pack-a", VISION];
    const out = reorderByCapabilities(models, new Set(["vision"]));
    expect(out[0]).toBe(VISION);
  });

  it("nested combo without vision stays behind", () => {
    const models = ["pack-b", VISION];
    const out = reorderByCapabilities(models, new Set(["vision"]), resolve);
    expect(out[0]).toBe(VISION);
    expect(out[1]).toBe("pack-b");
  });

  it("traverses multiple nesting levels", () => {
    const deep = createComboMemberResolver([
      { name: "outer", models: ["middle"] },
      { name: "middle", models: [TEXT, VISION] },
    ]);
    const out = reorderByCapabilities(["outer", TEXT], new Set(["vision"]), deep);
    expect(out[0]).toBe("outer");
  });

  it("terminates on self-reference and reference cycles", () => {
    const cyclic = createComboMemberResolver([
      { name: "self", models: ["self", TEXT] },
      { name: "ping", models: ["pong"] },
      { name: "pong", models: ["ping", TEXT] },
    ]);
    expect(reorderByCapabilities(["self", VISION], new Set(["vision"]), cyclic)[0]).toBe(VISION);
    expect(reorderByCapabilities(["ping", VISION], new Set(["vision"]), cyclic)[0]).toBe(VISION);
  });

  it("unknown bare names still score as plain models", () => {
    const out = reorderByCapabilities(["no-such-combo", VISION], new Set(["vision"]), resolve);
    expect(out[0]).toBe(VISION);
  });

  it("resolver ignores provider/model ids", () => {
    expect(resolve("anthropic/claude-sonnet-4.6")).toBeNull();
    expect(resolve("pack-a")).toEqual([TEXT, VISION]);
    expect(resolve("missing")).toBeNull();
  });
});
