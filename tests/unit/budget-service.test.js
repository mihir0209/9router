import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  keyRecord: {
    key: "sk-budget",
    budgetUsd: 10,
    budgetResetHours: 24,
    budgetWindowStartedAt: "2026-08-01T00:00:00.000Z",
    createdAt: "2026-08-01T00:00:00.000Z",
  },
  used: 0,
  saveRequestUsage: vi.fn(),
  get: vi.fn((sql, params) => sql.includes("provider = 'budget'") ? null : ({ used: mocks.used, params })),
}));

vi.mock("@/lib/db/driver.js", () => ({
  getAdapter: async () => ({ get: mocks.get }),
}));
vi.mock("@/lib/db/repos/apiKeysRepo.js", () => ({
  getApiKeyByKey: async () => mocks.keyRecord,
}));
vi.mock("@/lib/db/repos/usageRepo.js", () => ({
  saveRequestUsage: mocks.saveRequestUsage,
}));

import { __test__, budgetHeaders, debitBudget, getBudgetState } from "../../src/lib/budgetService.js";

describe("budget service", () => {
  beforeEach(() => {
    mocks.used = 0;
    mocks.keyRecord.budgetUsd = 10;
    __test__.clearReservations();
    mocks.saveRequestUsage.mockReset();
    mocks.get.mockClear();
    mocks.saveRequestUsage.mockImplementation(async (entry) => {
      await new Promise((resolve) => setTimeout(resolve, 5));
      mocks.used += entry.cost;
    });
  });

  it("sums durable spend in the current fixed reset window", async () => {
    mocks.used = 7.25;
    const state = await getBudgetState("sk-budget", new Date("2026-08-02T01:00:00.000Z"));

    expect(state).toEqual({
      enabled: true,
      limit: 10,
      used: 7.25,
      reserved: 0,
      remaining: 2.75,
      exceeded: false,
      resetsAt: "2026-08-03T00:00:00.000Z",
    });
    expect(mocks.get.mock.calls[0][1]).toEqual(["sk-budget", "2026-08-02T00:00:00.000Z"]);
  });

  it("serializes concurrent reservations and rejects overspend", async () => {
    const [first, second] = await Promise.all([
      debitBudget("sk-budget", { requestId: "one", reserveUsd: 6 }, { now: "2026-08-01T01:00:00.000Z" }),
      debitBudget("sk-budget", { requestId: "two", reserveUsd: 6 }, { now: "2026-08-01T01:00:00.000Z" }),
    ]);

    expect([first.reserved, second.reserved]).toEqual([true, false]);
    expect(mocks.saveRequestUsage).not.toHaveBeenCalled();
  });

  it("commits actual cost once and releases the reservation", async () => {
    await debitBudget("sk-budget", { requestId: "commit-me", reserveUsd: 6 }, { now: "2026-08-01T01:00:00.000Z" });
    const committed = await debitBudget("sk-budget", { requestId: "commit-me", commitUsd: 4 }, { now: "2026-08-01T01:01:00.000Z" });

    expect(committed).toMatchObject({ committed: true, used: 4, reserved: 0, remaining: 6 });
    expect(mocks.saveRequestUsage).toHaveBeenCalledWith(expect.objectContaining({
      model: "budget-commit:commit-me",
      cost: 4,
    }));
  });

  it("treats a zero-dollar budget as enabled and exceeded", async () => {
    mocks.keyRecord.budgetUsd = 0;
    const state = await getBudgetState("sk-budget", new Date("2026-08-01T01:00:00.000Z"));
    expect(state).toMatchObject({ enabled: true, limit: 0, remaining: 0, exceeded: true });
    mocks.keyRecord.budgetUsd = 10;
  });

  it("returns budget response headers", () => {
    expect(budgetHeaders({ enabled: true, limit: 10, used: 4, remaining: 6, resetsAt: "later" })).toEqual({
      "X-Budget-Limit": "10",
      "X-Budget-Used": "4",
      "X-Budget-Remaining": "6",
      "X-Budget-Reset": "later",
    });
  });
});
