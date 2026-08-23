import { describe, expect, it } from "vitest";
import { serializeApiKey } from "@/lib/apiKeys/serializers.js";

describe("API key serializers", () => {
  const workspaceKey = {
    id: "key-1",
    key: "sk-secret",
    ownerRef: "workspace-1",
    byokCredentialsCiphertext: "ciphertext",
    billingMode: "credits",
  };

  const localKey = {
    id: "key-2",
    key: "sk-local-secret",
    billingMode: "credits",
  };

  it("omits API key secret from workspace key reads", () => {
    expect(serializeApiKey(workspaceKey)).toEqual({
      id: "key-1",
      ownerRef: "workspace-1",
      billingMode: "credits",
    });
  });

  it("preserves API key secret for local 9router dashboard keys", () => {
    expect(serializeApiKey(localKey)).toEqual({
      id: "key-2",
      key: "sk-local-secret",
      billingMode: "credits",
    });
  });

  it("includes only the API key secret when minting", () => {
    expect(serializeApiKey(workspaceKey, { includeSecret: true })).toEqual({
      id: "key-1",
      key: "sk-secret",
      ownerRef: "workspace-1",
      billingMode: "credits",
    });
  });
});
