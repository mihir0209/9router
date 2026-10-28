import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  order: [],
  importDb: vi.fn(),
  exportDb: vi.fn(),
  getSettings: vi.fn(),
  flush: vi.fn(),
  getAdapter: vi.fn(),
  verifyDashboardPassword: vi.fn(),
  hasValidAdminApiToken: vi.fn(),
  applyOutboundProxyEnv: vi.fn(),
}));

vi.mock("@/lib/localDb", () => ({
  importDb: mocks.importDb,
  exportDb: mocks.exportDb,
  getSettings: mocks.getSettings,
}));
vi.mock("@/lib/db/driver.js", () => ({ getAdapter: mocks.getAdapter }));
vi.mock("@/lib/network/outboundProxy", () => ({ applyOutboundProxyEnv: mocks.applyOutboundProxyEnv }));
vi.mock("@/lib/auth/dashboardSession", () => ({ verifyDashboardPassword: mocks.verifyDashboardPassword }));
vi.mock("@/lib/auth/adminApiToken", () => ({ hasValidAdminApiToken: mocks.hasValidAdminApiToken }));

const { POST } = await import("@/app/api/settings/database/route.js");

function post(body, headers = {}) {
  return new Request("http://localhost/api/settings/database", {
    method: "POST",
    headers: { "content-type": "application/json", ...headers },
    body: JSON.stringify(body),
  });
}

const counts = { settings: 1, providerConnections: 2, apiKeys: 0, combos: 1 };

beforeEach(() => {
  vi.clearAllMocks();
  mocks.order.length = 0;
  mocks.hasValidAdminApiToken.mockResolvedValue(false);
  mocks.verifyDashboardPassword.mockResolvedValue(true);
  mocks.getSettings.mockResolvedValue({ password: "hash", oidcClientSecret: "secret" });
  mocks.importDb.mockImplementation(async () => {
    mocks.order.push("importDb");
    return { success: true, counts };
  });
  mocks.flush.mockImplementation(async () => {
    mocks.order.push("flush");
  });
  mocks.getAdapter.mockResolvedValue({ flush: mocks.flush });
});

describe("POST /api/settings/database", () => {
  it("returns success with counts and a duration, flushing after importDb", async () => {
    const res = await POST(post({ password: "pw", combos: [{ id: "c1" }], settings: { a: 1 } }));
    const body = await res.json();

    expect(res.status).toBe(200);
    expect(body.success).toBe(true);
    expect(body.counts).toEqual(counts);
    expect(typeof body.durationMs).toBe("number");
    expect(body.durationMs).toBeGreaterThanOrEqual(0);
    expect(mocks.order).toEqual(["importDb", "flush"]);
    // The response is timing and counts only: no echo of the payload or secrets.
    expect(Object.keys(body).sort()).toEqual(["counts", "durationMs", "success"]);
  });

  it("passes the payload without the password and keeps protected settings", async () => {
    await POST(post({ password: "pw", settings: { password: "evil", oidcClientSecret: "evil", a: 1 } }));

    const imported = mocks.importDb.mock.calls[0][0];
    expect(imported).not.toHaveProperty("password", "pw");
    expect(imported.settings).toEqual({ password: "hash", oidcClientSecret: "secret", a: 1 });
  });

  it("returns 401 and never imports when authorisation fails", async () => {
    mocks.verifyDashboardPassword.mockResolvedValue(false);

    const res = await POST(post({ password: "wrong", combos: [] }));

    expect(res.status).toBe(401);
    expect((await res.json()).error).toMatch(/Invalid password/);
    expect(mocks.importDb).not.toHaveBeenCalled();
    expect(mocks.flush).not.toHaveBeenCalled();
  });

  it("accepts a valid admin API token without a password", async () => {
    mocks.hasValidAdminApiToken.mockResolvedValue(true);
    mocks.verifyDashboardPassword.mockResolvedValue(false);

    const res = await POST(post({ combos: [] }));

    expect(res.status).toBe(200);
    expect(mocks.importDb).toHaveBeenCalledTimes(1);
  });

  it("returns 400 with the error message when importDb throws", async () => {
    mocks.importDb.mockRejectedValue(new Error("Invalid database payload"));

    const res = await POST(post({ password: "pw" }));

    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "Invalid database payload" });
    expect(mocks.flush).not.toHaveBeenCalled();
  });

  it("returns 400 when the flush fails, so a non-durable restore is not reported as success", async () => {
    mocks.flush.mockRejectedValue(new Error("flush failed"));

    const res = await POST(post({ password: "pw" }));

    expect(res.status).toBe(400);
    expect((await res.json()).error).toBe("flush failed");
  });
});
