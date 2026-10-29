// /api/cron/flush: with CRON_SECRET set it needs the secret (Bearer or x-cron-secret); the scheduled
// handler built by scripts/build-cloudflare.mjs sends `Authorization: Bearer <env.CRON_SECRET>`.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/lib/db/index.js", () => ({
  flushAnalyticsBuffer: async () => {},
  flushRequestDetailsBuffer: async () => 0,
  getAnalyticsBufferSize: () => 0,
  isAnalyticsBufferingEnabled: async () => true,
}));

const call = async (method, headers = {}) => {
  const mod = await import("../../src/app/api/cron/flush/route.js");
  return mod[method](new Request("https://x.test/api/cron/flush", { method, headers }));
};

describe("cron flush authorization", () => {
  beforeEach(() => { process.env.CRON_SECRET = "s3cret-value"; });
  afterEach(() => { delete process.env.CRON_SECRET; });

  it("rejects a missing, wrong or prefix-only secret with 401", async () => {
    expect((await call("POST")).status).toBe(401);
    expect((await call("GET")).status).toBe(401);
    expect((await call("POST", { authorization: "Bearer nope" })).status).toBe(401);
    expect((await call("POST", { authorization: "Bearer s3cret" })).status).toBe(401);
    expect((await call("POST", { authorization: "Bearer s3cret-value-extra" })).status).toBe(401);
  });

  it("accepts the secret as Bearer or x-cron-secret", async () => {
    expect((await call("POST", { authorization: "Bearer s3cret-value" })).status).toBe(200);
    expect((await call("GET", { "x-cron-secret": "s3cret-value" })).status).toBe(200);
  });

  it("is open when no secret is configured (documented; set CRON_SECRET in production)", async () => {
    delete process.env.CRON_SECRET;
    expect((await call("POST")).status).toBe(200);
  });
});
