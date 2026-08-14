import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  verifyDashboardPassword: vi.fn(),
}));

vi.mock("@/lib/auth/dashboardSession", () => ({
  verifyDashboardPassword: mocks.verifyDashboardPassword,
}));

const { hasValidAdminApiToken } = await import("@/lib/auth/adminApiToken.js");
const originalToken = process.env.ADMIN_API_TOKEN;

function request(token) {
  return { headers: new Headers(token ? { authorization: `Bearer ${token}` } : {}) };
}

beforeEach(() => {
  process.env.ADMIN_API_TOKEN = "service-secret";
  mocks.verifyDashboardPassword.mockResolvedValue(false);
});

afterEach(() => {
  vi.clearAllMocks();
  if (originalToken === undefined) delete process.env.ADMIN_API_TOKEN;
  else process.env.ADMIN_API_TOKEN = originalToken;
});

describe("ADMIN_API_TOKEN", () => {
  it("accepts an exact bearer token", async () => {
    expect(await hasValidAdminApiToken(request("service-secret"))).toBe(true);
  });

  it("rejects a mismatched token", async () => {
    expect(await hasValidAdminApiToken(request("wrong"))).toBe(false);
  });

  it("fails closed while the default dashboard password is valid", async () => {
    mocks.verifyDashboardPassword.mockResolvedValue(true);

    expect(await hasValidAdminApiToken(request("service-secret"))).toBe(false);
  });
});
