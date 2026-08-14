import { beforeEach, describe, expect, it, vi } from "vitest";

const db = vi.hoisted(() => ({
  getApiKeyByKey: vi.fn(),
  getProviderConnections: vi.fn(),
  validateApiKey: vi.fn(),
  updateProviderConnection: vi.fn(),
  getSettings: vi.fn(),
  getProxyPools: vi.fn(),
  getProviderNodeById: vi.fn(),
}));

vi.mock("@/lib/localDb", () => db);
vi.mock("@/lib/network/connectionProxy", () => ({ resolveConnectionProxyConfig: vi.fn(), pickProxyPoolId: vi.fn() }));
vi.mock("@/lib/proxy/quarantine", () => ({ getProxyPoolQuarantine: vi.fn(), isProxyQuarantined: vi.fn() }));

import { encryptByokCredential } from "@/lib/byokCrypto.js";
import {
  ByokCredentialsError,
  byokUpstreamErrorResponse,
  getProviderCredentials,
} from "@/sse/services/auth.js";

beforeEach(() => {
  vi.clearAllMocks();
  process.env.BYOK_ENC_KEY = Buffer.alloc(32, 9).toString("base64");
});

describe("BYOK credential resolution", () => {
  it("returns only the key-owned credential and never reads platform accounts", async () => {
    const keyRecord = {
      id: "client-1",
      billingMode: "byok",
      byokProvider: "openai",
      byokCredsEncVersion: 1,
      byokCredentialsCiphertext: encryptByokCredential("sk-owned", "openai"),
    };

    const credentials = await getProviderCredentials("openai", null, "gpt", { keyRecord });

    expect(credentials).toMatchObject({ apiKey: "sk-owned", connectionId: "byok:client-1", isByok: true });
    expect(db.getProviderConnections).not.toHaveBeenCalled();
  });

  it("does not fall back when the BYOK provider does not match", async () => {
    const result = await getProviderCredentials("anthropic", null, null, {
      keyRecord: { id: "client-1", billingMode: "byok", byokProvider: "openai", byokCredsEncVersion: 1, byokCredentialsCiphertext: "x" },
    });

    expect(result).toBeInstanceOf(ByokCredentialsError);
    expect(result.code).toBe("user_credentials_invalid");
    expect(db.getProviderConnections).not.toHaveBeenCalled();
  });

  it("normalizes an upstream rejection to the public typed error", async () => {
    const response = byokUpstreamErrorResponse({ isByok: true }, 401);
    expect(response.status).toBe(401);
    expect(await response.json()).toMatchObject({ error: { code: "user_credentials_invalid" } });
  });
});
