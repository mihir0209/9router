import { describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ insertedCost: null }));

vi.mock("@/lib/db/driver.js", () => ({
  getAdapter: async () => ({
    get: vi.fn((sql) => sql.includes("totalRequestsLifetime") ? { value: "0" } : null),
    run: vi.fn((sql, params) => {
      if (sql.includes("INSERT INTO usageHistory")) mocks.insertedCost = params[8];
      return { changes: 1 };
    }),
    transaction: (operation) => operation(),
    all: vi.fn(() => []),
  }),
}));
import { saveRequestUsage } from "../../src/lib/db/repos/usageRepo.js";

describe("saveRequestUsage forceZeroCost", () => {
  it("persists zero cost without consulting pricing", async () => {
    await saveRequestUsage({
      timestamp: "2026-08-05T00:00:00.000Z",
      provider: "openai",
      model: "gpt-test",
      apiKey: "sk-byok",
      tokens: { prompt_tokens: 10 },
      forceZeroCost: true,
    });

    expect(mocks.insertedCost).toBe(0);
  });
});
