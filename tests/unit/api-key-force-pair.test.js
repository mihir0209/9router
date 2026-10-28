import { describe, expect, it, vi } from "vitest";

vi.mock("@/sse/services/apiKeyEnforcement.js", () => ({ resolveRequestedModels: vi.fn() }));
const { findForcePairMismatch } = await import("@/lib/apiKeys/forcePair.js");

const resolver = (providers) => vi.fn(async () => ({ providers }));

describe("findForcePairMismatch", () => {
  it("is a no-op unless both pins are set", async () => {
    const resolve = resolver(["openai"]);
    expect(await findForcePairMismatch({ forceProvider: "", forceModel: "openai/gpt-4o" }, resolve)).toBeNull();
    expect(await findForcePairMismatch({ forceProvider: "openai", forceModel: "" }, resolve)).toBeNull();
    expect(resolve).not.toHaveBeenCalled();
  });

  it("accepts a model that resolves only to the pinned provider", async () => {
    const resolve = resolver(["openai"]);
    expect(await findForcePairMismatch({ forceProvider: "openai", forceModel: "gpt-4o" }, resolve)).toBeNull();
    expect(resolve).toHaveBeenCalledWith("gpt-4o", null);
  });

  it("rejects a model that resolves to another provider", async () => {
    const msg = await findForcePairMismatch({ forceProvider: "anthropic", forceModel: "openai/gpt-4o" }, resolver(["openai"]));
    expect(msg).toMatch(/routes to provider 'openai'.*forceProvider 'anthropic'/);
  });

  it("rejects a combo whose members span providers (runtime would 403)", async () => {
    expect(await findForcePairMismatch({ forceProvider: "openai", forceModel: "mix" }, resolver(["openai", "anthropic"]))).toMatch(/does not match/);
  });

  it("fails closed when the model does not resolve to any provider", async () => {
    expect(await findForcePairMismatch({ forceProvider: "openai", forceModel: "mystery" }, resolver([]))).toMatch(/does not resolve to a provider/);
  });
});
