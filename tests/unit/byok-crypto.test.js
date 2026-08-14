import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { decryptByokCredential, encryptByokCredential } from "@/lib/byokCrypto.js";

const originalKey = process.env.BYOK_ENC_KEY;

beforeEach(() => {
  process.env.BYOK_ENC_KEY = Buffer.alloc(32, 7).toString("base64");
});

afterEach(() => {
  if (originalKey === undefined) delete process.env.BYOK_ENC_KEY;
  else process.env.BYOK_ENC_KEY = originalKey;
});

describe("BYOK credential envelopes", () => {
  it("round trips an API key without placing plaintext in the envelope", () => {
    const envelope = encryptByokCredential("provider-secret", "openai");

    expect(envelope).not.toContain("provider-secret");
    expect(JSON.parse(envelope)).toMatchObject({ v: 1, alg: "A256GCM" });
    expect(decryptByokCredential(envelope, "openai", 1)).toBe("provider-secret");
  });

  it("authenticates the provider binding", () => {
    const envelope = encryptByokCredential("provider-secret", "openai");
    expect(() => decryptByokCredential(envelope, "anthropic", 1)).toThrow();
  });

  it("requires an exact 256-bit encryption key", () => {
    process.env.BYOK_ENC_KEY = Buffer.alloc(16).toString("base64");
    expect(() => encryptByokCredential("provider-secret", "openai")).toThrow(/32-byte/);
  });
});
