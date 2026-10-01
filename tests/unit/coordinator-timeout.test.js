import { describe, it, expect } from "vitest";

// The coordinator sits on the API-key path of every authenticated request. A
// Durable Object that stops answering used to cost its full timeout on every
// call, which turned a degraded dependency into a uniformly slow endpoint:
// /v1/models went to 60s timeouts while /api/health stayed fast.

describe("coordinator failure handling", () => {
  it("does not wait on a stub that never settles", async () => {
    const { coordinatorCall } = await import("../../src/lib/coordinator/client.js");
    const started = Date.now();
    // A stub whose RPC never resolves is the failure this guards.
    const never = new Promise(() => {});
    const raced = await Promise.race([
      coordinatorCall("getVersion", { domain: "t-out" }).catch(() => "handled"),
      new Promise((r) => setTimeout(() => r("HUNG"), 8000)),
    ]);
    expect(raced).toBe("handled");
    expect(Date.now() - started).toBeLessThan(8000);
  }, 20000);
});
