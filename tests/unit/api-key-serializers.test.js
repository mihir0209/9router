import { describe, expect, it } from "vitest";
import { serializeApiKey } from "@/lib/apiKeys/serializers.js";

describe("API key serializers", () => {
  const key = {
    id: "key-1",
    key: "sk-secret",
    ownerRef: "workspace-1",
    byokCredentialsCiphertext: "ciphertext",
    billingMode: "credits",
  };

  it("omits API key and BYOK secrets from reads", () => {
    expect(serializeApiKey(key)).toEqual({
      id: "key-1",
      ownerRef: "workspace-1",
      billingMode: "credits",
    });
  });

  it("includes only the API key secret when minting", () => {
    expect(serializeApiKey(key, { includeSecret: true })).toEqual({
      id: "key-1",
      key: "sk-secret",
      ownerRef: "workspace-1",
      billingMode: "credits",
    });
  });
});
