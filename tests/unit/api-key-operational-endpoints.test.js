import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

let tempDir;
const originalDataDir = process.env.DATA_DIR;

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-key-ops-"));
  process.env.DATA_DIR = tempDir;
  delete global._dbAdapter;
  vi.resetModules();
});

afterEach(() => {
  try { global._dbAdapter?.instance?.close?.(); } catch {}
  delete global._dbAdapter;
  fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
});

describe("API key operational endpoints", () => {
  it("PUT regenerate returns a replacement key and incremented version", async () => {
    const { createApiKey } = await import("@/lib/db/index.js");
    const key = await createApiKey("ops", "machine-test");
    const { PUT } = await import("@/app/api/keys/[id]/regenerate/route.js");

    const response = await PUT(new Request("http://localhost/api/keys/x/regenerate", { method: "PUT" }), {
      params: Promise.resolve({ id: key.id }),
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.key).not.toBe(key.key);
    expect(body.regenerateVersion).toBe(1);
    expect(body.byokCredentialsCiphertext).toBeUndefined();
  });

  it("GET usage returns daily request, token, and cost aggregates within the range", async () => {
    const { createApiKey } = await import("@/lib/db/index.js");
    const { getAdapter } = await import("@/lib/db/driver.js");
    const key = await createApiKey("ops", "machine-test");
    const db = await getAdapter();
    const insert = (timestamp, prompt, completion, cost) => db.run(
      `INSERT INTO usageHistory(timestamp, apiKey, promptTokens, completionTokens, cost) VALUES(?, ?, ?, ?, ?)`,
      [timestamp, key.key, prompt, completion, cost]
    );
    insert("2026-08-01T01:00:00.000Z", 10, 4, 0.1);
    insert("2026-08-01T02:00:00.000Z", 20, 6, 0.2);
    insert("2026-08-02T01:00:00.000Z", 30, 8, 0.3);

    const { GET } = await import("@/app/api/usage/api-keys/[id]/route.js");
    const response = await GET(
      new Request("http://localhost/api/usage/api-keys/x?from=2026-08-01&to=2026-08-01"),
      { params: Promise.resolve({ id: key.id }) }
    );
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.rows).toEqual([{
      day: "2026-08-01",
      requests: 2,
      tokensIn: 30,
      tokensOut: 10,
      costUsd: 0.30000000000000004,
    }]);
  });

  it("sets and atomically clears an encrypted BYOK credential", async () => {
    process.env.BYOK_ENC_KEY = Buffer.alloc(32, 4).toString("base64");
    const { createApiKey, getApiKeyById } = await import("@/lib/db/index.js");
    const { encryptByokCredential } = await import("@/lib/byokCrypto.js");
    const key = await createApiKey("ops", "machine-test");
    const { PUT } = await import("@/app/api/keys/[id]/byok/route.js");
    const ciphertext = encryptByokCredential("user-secret", "openai");

    const setResponse = await PUT(new Request("http://localhost/api/keys/x/byok", {
      method: "PUT",
      body: JSON.stringify({ provider: "openai", credentialsCiphertext: ciphertext }),
    }), { params: { id: key.id } });
    expect(setResponse.status).toBe(200);
    expect(await setResponse.json()).toEqual({ billingMode: "byok" });

    const clearResponse = await PUT(new Request("http://localhost/api/keys/x/byok", {
      method: "PUT",
      body: JSON.stringify({ provider: "openai", credentialsCiphertext: null }),
    }), { params: { id: key.id } });
    expect(clearResponse.status).toBe(200);
    expect(await getApiKeyById(key.id)).toMatchObject({
      billingMode: "credits",
      byokProvider: null,
      byokCredentialsCiphertext: null,
      byokCredsEncVersion: null,
    });
  });

  it("rejects invalid and reversed usage ranges", async () => {
    const { createApiKey } = await import("@/lib/db/index.js");
    const key = await createApiKey("ops", "machine-test");
    const { GET } = await import("@/app/api/usage/api-keys/[id]/route.js");

    const invalid = await GET(new Request("http://localhost/api/usage/api-keys/x?from=nope"), { params: { id: key.id } });
    const reversed = await GET(new Request("http://localhost/api/usage/api-keys/x?from=2026-08-02&to=2026-08-01"), { params: { id: key.id } });

    expect(invalid.status).toBe(400);
    expect(reversed.status).toBe(400);
  });
});
