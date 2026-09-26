import { describe, it, expect, beforeEach, vi } from "vitest";

// BYOK gaps closed here, all found by auditing every getProviderCredentials
// call site: systemone ignored the keyRecord entirely, the video poll could not
// resolve a BYOK connection, and BYOK calls reported no outcome at all.

const mocks = vi.hoisted(() => ({
  validateAndEnforceApiKey: vi.fn(),
  getProviderCredentials: vi.fn(),
  markAccountUnavailable: vi.fn(),
  clearAccountError: vi.fn(),
  checkAndRefreshToken: vi.fn(async (_p, c) => c),
  saveRequestUsage: vi.fn(async () => {}),
  handleSystemoneCore: vi.fn(),
  getProviderConnectionById: vi.fn(async () => null),
  handleVideoProxyCore: vi.fn(),
  getVideoConfig: vi.fn(() => ({})),
  recordRequestOutcome: vi.fn(),
  recordProviderOutcome: vi.fn(async () => {}),
  decryptByokCredential: vi.fn(() => "sk-caller-owned"),
  guardApiKeyResponse: vi.fn((response) => response),
}));

vi.mock("../../src/sse/services/validateApiKey.js", () => ({ validateAndEnforceApiKey: mocks.validateAndEnforceApiKey }));
vi.mock("../../src/sse/services/auth.js", () => ({
  getProviderCredentials: mocks.getProviderCredentials,
  markAccountUnavailable: mocks.markAccountUnavailable,
  clearAccountError: mocks.clearAccountError,
}));
vi.mock("../../src/sse/services/tokenRefresh.js", () => ({ checkAndRefreshToken: mocks.checkAndRefreshToken }));
vi.mock("@/lib/usageDb.js", () => ({ saveRequestUsage: mocks.saveRequestUsage }));
vi.mock("open-sse/handlers/systemoneCore.js", () => ({ handleSystemoneCore: mocks.handleSystemoneCore }));
vi.mock("open-sse/handlers/videoCore.js", () => ({
  handleVideoProxyCore: mocks.handleVideoProxyCore,
  getVideoConfig: mocks.getVideoConfig,
  sanitizeSecrets: vi.fn((x) => x),
}));
vi.mock("@/lib/localDb", () => ({
  getProviderConnectionById: mocks.getProviderConnectionById,
  getSettings: vi.fn(async () => ({ requireApiKey: true })),
  getApiKeyById: vi.fn(async () => null),
  getApiKeyByKey: vi.fn(async () => null),
  getCombos: vi.fn(async () => []),
}));
vi.mock("@/lib/observability/metrics.js", () => ({ recordRequestOutcome: mocks.recordRequestOutcome }));
vi.mock("@/lib/coordinator/client.js", () => ({
  recordProviderOutcome: mocks.recordProviderOutcome,
  coordinatorCall: vi.fn(async () => ({})),
  getAdaptiveScores: vi.fn(async () => ({})),
  scoreProviderEntries: vi.fn((x) => x),
}));
vi.mock("@/lib/byokCrypto.js", () => ({ decryptByokCredential: mocks.decryptByokCredential }));
vi.mock("../../src/sse/services/apiKeyStreamGuard.js", () => ({ guardApiKeyResponse: mocks.guardApiKeyResponse }));

const { handleSystemone } = await import("../../src/sse/handlers/systemone.js");
const { handleVideoGet } = await import("../../src/sse/handlers/videoGeneration.js");
// auth.js is mocked above for the handler tests; the BYOK outcome assertions
// need the real one, whose own dependencies stay mocked.
const { getProviderCredentials: realGetProviderCredentials } = await vi.importActual("../../src/sse/services/auth.js");

// credentialsErrorResponse identifies the error by constructor name, so the
// stand-in has to be a class actually called ByokCredentialsError.
class ByokCredentialsError extends Error {
  constructor(message = "The BYOK credential is invalid") {
    super(message);
    this.name = "ByokCredentialsError";
    this.code = "user_credentials_invalid";
  }
}

const byokKeyRecord = (over = {}) => ({
  id: "key-9",
  ownerRef: "acme",
  isActive: true,
  billingMode: "byok",
  byokProvider: "openai",
  byokCredentialsCiphertext: "enc",
  byokCredsEncVersion: 1,
  ...over,
});

const jsonRequest = (body, headers = {}) =>
  new Request("https://gateway.test/v1/systemone", {
    method: "POST",
    headers: { "content-type": "application/json", ...headers },
    body: JSON.stringify(body),
  });

beforeEach(() => {
  vi.clearAllMocks();
  mocks.checkAndRefreshToken.mockImplementation(async (_p, c) => c);
  mocks.saveRequestUsage.mockResolvedValue(undefined);
  mocks.getVideoConfig.mockReturnValue({});
  mocks.validateAndEnforceApiKey.mockResolvedValue({
    allowed: true,
    apiKey: "sk-client",
    keyRecord: null,
    release: null,
  });
});

describe("systemone — shared auth enforcement", () => {
  const body = { model: "openai/gpt-4o", state: {}, questions: { a: "q" } };

  it("uses the shared enforcement path instead of a boolean key check", async () => {
    mocks.getProviderCredentials.mockResolvedValue({ apiKey: "k", connectionId: "c1", connectionName: "acct" });
    mocks.handleSystemoneCore.mockResolvedValue({ success: true, response: new Response("{}") });

    await handleSystemone(jsonRequest(body, { authorization: "Bearer sk-client" }));

    // The old code called getSettings + isValidApiKey; now the one shared gate runs.
    expect(mocks.validateAndEnforceApiKey).toHaveBeenCalledOnce();
  });

  it("passes the keyRecord through so a BYOK key is honoured", async () => {
    const keyRecord = byokKeyRecord();
    mocks.validateAndEnforceApiKey.mockResolvedValue({ allowed: true, apiKey: "sk-client", keyRecord, release: null });
    mocks.getProviderCredentials.mockResolvedValue({ apiKey: "sk-caller-owned", connectionId: "byok:key-9", isByok: true });
    mocks.handleSystemoneCore.mockResolvedValue({ success: true, response: new Response("{}") });

    await handleSystemone(jsonRequest(body, { authorization: "Bearer sk-client" }));

    // The 4th argument is the whole point: without it getProviderCredentials sees
    // no keyRecord and silently uses a pooled platform account.
    expect(mocks.getProviderCredentials).toHaveBeenCalledWith(
      "openai", expect.anything(), "gpt-4o", expect.objectContaining({ keyRecord })
    );
  });

  it("returns a 401 for a BYOK provider mismatch instead of a misleading 400", async () => {
    mocks.validateAndEnforceApiKey.mockResolvedValue({
      allowed: true, apiKey: "sk-client", keyRecord: byokKeyRecord({ byokProvider: "anthropic" }), release: null,
    });
    mocks.getProviderCredentials.mockResolvedValue(
      new ByokCredentialsError("No valid BYOK credential for provider: openai")
    );

    const res = await handleSystemone(jsonRequest(body, { authorization: "Bearer sk-client" }));
    // A truthy error object must be checked before the !credentials branch.
    expect(res.status).toBe(401);
  });

  it("rejects a denied key before touching credentials", async () => {
    mocks.validateAndEnforceApiKey.mockResolvedValue({ allowed: false, response: new Response("denied", { status: 403 }) });
    const res = await handleSystemone(jsonRequest(body));
    expect(res.status).toBe(403);
    expect(mocks.getProviderCredentials).not.toHaveBeenCalled();
  });

  it("attributes usage to the api key id", async () => {
    const keyRecord = { id: "key-42", ownerRef: "acme", isActive: true };
    mocks.validateAndEnforceApiKey.mockResolvedValue({ allowed: true, apiKey: "sk-client", keyRecord, release: null });
    mocks.getProviderCredentials.mockResolvedValue({ apiKey: "k", connectionId: "c1", connectionName: "acct" });
    mocks.handleSystemoneCore.mockResolvedValue({
      success: true,
      response: new Response("{}"),
      usage: { prompt_tokens: 10, completion_tokens: 5 },
    });

    await handleSystemone(jsonRequest(body));
    expect(mocks.saveRequestUsage).toHaveBeenCalledWith(expect.objectContaining({ apiKeyId: "key-42" }));
  });
});

describe("video poll — BYOK provider resolution", () => {
  const poll = (headers = {}) =>
    handleVideoGet(new Request("https://gateway.test/v1/videos/job-1", { headers }), "job-1");

  it("resolves the provider from the BYOK key rather than defaulting to xAI", async () => {
    const keyRecord = byokKeyRecord({ byokProvider: "openrouter" });
    mocks.validateAndEnforceApiKey.mockResolvedValue({ allowed: true, apiKey: "sk-client", keyRecord, release: null });
    mocks.getVideoConfig.mockImplementation((p) => (p === "openrouter" ? { baseUrl: "https://x" } : {}));
    mocks.getProviderCredentials.mockResolvedValue({ apiKey: "sk-caller-owned", connectionId: "byok:key-9", isByok: true });
    mocks.handleVideoProxyCore.mockResolvedValue({ success: true, response: new Response("{}") });

    await poll({ "x-connection-id": "byok:key-9" });

    // Before the fix the byok: id never resolved to a connection, so this fell
    // through to DEFAULT_VIDEO_PROVIDER and the strict provider check rejected it.
    expect(mocks.getProviderCredentials).toHaveBeenCalledWith(
      "openrouter", null, null, expect.objectContaining({ keyRecord })
    );
    expect(mocks.getProviderConnectionById).not.toHaveBeenCalled();
  });

  it("still resolves a real pinned connection", async () => {
    mocks.validateAndEnforceApiKey.mockResolvedValue({ allowed: true, apiKey: "sk-client", keyRecord: null, release: null });
    mocks.getVideoConfig.mockImplementation((p) => (p === "vertex" ? { baseUrl: "https://v" } : {}));
    mocks.getProviderConnectionById.mockResolvedValue({ provider: "vertex" });
    mocks.getProviderCredentials.mockResolvedValue({ apiKey: "k", connectionId: "c1" });
    mocks.handleVideoProxyCore.mockResolvedValue({ success: true, response: new Response("{}") });

    await poll({ "x-connection-id": "c1" });
    expect(mocks.getProviderCredentials).toHaveBeenCalledWith("vertex", null, null, expect.anything());
  });
});

describe("BYOK outcome reporting", () => {
  it("reports BYOK traffic to metrics but never to platform health scoring", async () => {
    const keyRecord = byokKeyRecord();
    const creds = await realGetProviderCredentials("openai", null, "gpt-4o", { keyRecord });

    expect(creds.isByok).toBe(true);
    await creds.onProviderOutcome({ success: false, status: 500, latencyMs: 42 });

    // Visible per-tenant in metrics...
    expect(mocks.recordRequestOutcome).toHaveBeenCalledWith(expect.objectContaining({
      tenant: "acme", provider: "openai", success: false, status: 500, latencyMs: 42,
    }));
    // ...but the platform's account health must not be touched: a caller
    // supplying their own key says nothing about our pooled credentials.
    expect(mocks.recordProviderOutcome).not.toHaveBeenCalled();
  });
});
