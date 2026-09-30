import { describe, it, expect } from "vitest";
import { createInMemoryOps, parseSelect } from "../../src/lib/db/adapters/memoryEngine.js";

// The in-memory engine is what serves Cloudflare D1 and Postgres. It failed
// silently: a clause it did not recognise made the whole statement fail to
// parse, and `all` returned [] with no error. So the usage dashboard reported
// zero traffic on Workers while SQLite, which runs the real SQL, looked fine.

function engine(rows) {
  const state = { tables: { usageHistory: rows, connections: [], settings: [] } };
  return createInMemoryOps(state, {});
}

const USAGE = [
  { apiKey: "k1", timestamp: "2026-08-01T01:00:00.000Z", promptTokens: 10, completionTokens: 4, cost: 0.1 },
  { apiKey: "k1", timestamp: "2026-08-01T02:00:00.000Z", promptTokens: 20, completionTokens: 6, cost: 0.2 },
  { apiKey: "k1", timestamp: "2026-08-02T01:00:00.000Z", promptTokens: 30, completionTokens: 8, cost: 0.3 },
  { apiKey: "k2", timestamp: "2026-08-02T03:00:00.000Z", promptTokens: 99, completionTokens: 1, cost: 9.9 },
];

const DAILY = `SELECT substr(timestamp, 1, 10) AS date,
       COUNT(*) AS requests,
       COALESCE(SUM(promptTokens), 0) AS promptTokens,
       COALESCE(SUM(completionTokens), 0) AS completionTokens,
       COALESCE(SUM(promptTokens), 0) + COALESCE(SUM(completionTokens), 0) AS tokens,
       COALESCE(SUM(cost), 0) AS cost
  FROM usageHistory
 WHERE apiKey = ?
 GROUP BY substr(timestamp, 1, 10)
 ORDER BY date ASC`;

describe("memory engine GROUP BY", () => {
  it("parses the clause instead of failing the whole statement", () => {
    const parsed = parseSelect("SELECT a, COUNT(*) AS c FROM t GROUP BY a ORDER BY c DESC");
    expect(parsed).not.toBeNull();
    expect(parsed.groupBy).toBe("a");
    expect(parsed.order).toBe("c DESC");
  });

  it("buckets rows and returns one row per group", () => {
    const ops = engine(USAGE);
    const rows = ops.all(DAILY, ["k1"]);
    expect(rows).toEqual([
      { date: "2026-08-01", requests: 2, promptTokens: 30, completionTokens: 10, tokens: 40, cost: 0.30000000000000004 },
      { date: "2026-08-02", requests: 1, promptTokens: 30, completionTokens: 8, tokens: 38, cost: 0.3 },
    ]);
  });

  it("adds every aggregate in a compound expression, not just the first", () => {
    // COALESCE(SUM(a),0) + COALESCE(SUM(b),0) returned only the a term.
    const ops = engine(USAGE);
    const [row] = ops.all(DAILY, ["k1"]);
    expect(row.tokens).toBe(40);
  });

  it("keeps groups of different types apart", () => {
    const ops = engine([{ g: 1, v: 10 }, { g: "1", v: 20 }]);
    const rows = ops.all("SELECT g, SUM(v) AS total FROM usageHistory GROUP BY g");
    expect(rows).toHaveLength(2);
    expect(rows.map((r) => r.total).sort()).toEqual([10, 20]);
  });

  it("collapses a whole-table aggregate to a single row with no GROUP BY", () => {
    const ops = engine(USAGE);
    expect(ops.all("SELECT COUNT(*) AS c FROM usageHistory")).toEqual([{ c: 4 }]);
  });

  it("orders by a select alias", () => {
    const ops = engine(USAGE);
    const rows = ops.all(DAILY, ["k1"]);
    expect(rows.map((r) => r.date)).toEqual(["2026-08-01", "2026-08-02"]);
  });
});

describe("memory engine select expressions", () => {
  it("projects a per-row expression instead of dropping it", () => {
    const ops = engine(USAGE);
    const rows = ops.all("SELECT substr(timestamp, 1, 10) AS day FROM usageHistory LIMIT 1", []);
    expect(rows[0].day).toBe("2026-08-01");
  });

  it("does not let a greedy COALESCE swallow a compound expression", () => {
    const ops = engine([{ a: null, b: 7 }]);
    // Evaluated to 0 (the first argument) instead of 7.
    expect(ops.all("SELECT COALESCE(a, 0) + COALESCE(b, 0) AS total FROM usageHistory", [])[0].total).toBe(7);
  });

  it("still handles *, plain columns and DISTINCT", () => {
    const ops = engine([{ g: "x" }, { g: "y" }, { g: "x" }]);
    expect(ops.all("SELECT * FROM usageHistory")).toHaveLength(3);
    expect(ops.all("SELECT g FROM usageHistory LIMIT 1")[0].g).toBe("x");
    expect(ops.all("SELECT DISTINCT g FROM usageHistory")).toHaveLength(2);
  });

  it("keeps MAX semantics, including null for an empty set", () => {
    const ops = engine([{ p: 3 }, { p: 9 }]);
    expect(ops.all("SELECT MAX(p) AS m FROM usageHistory")).toEqual([{ m: 9 }]);
    expect(ops.all("SELECT MAX(p) AS m FROM usageHistory WHERE p > 100")).toEqual([{ m: null }]);
  });
});

describe("capability memoization", () => {
  it("returns equal results on repeat calls", async () => {
    const { getCapabilitiesForModel } = await import("../../open-sse/providers/capabilities.js");
    const a = getCapabilitiesForModel("anthropic", "claude-sonnet-4");
    const b = getCapabilitiesForModel("anthropic", "claude-sonnet-4");
    expect(b).toEqual(a);
  });

  it("hands back a copy, so one caller cannot mutate another's result", async () => {
    const { getCapabilitiesForModel } = await import("../../open-sse/providers/capabilities.js");
    const first = getCapabilitiesForModel("openai", "gpt-4o");
    first.contextWindow = 1;
    first.injected = true;
    const second = getCapabilitiesForModel("openai", "gpt-4o");
    // A shared object here would let one request poison every later one.
    expect(second.contextWindow).not.toBe(1);
    expect(second.injected).toBeUndefined();
  });

  it("still differentiates models and providers", async () => {
    const { getCapabilitiesForModel } = await import("../../open-sse/providers/capabilities.js");
    const key = `${getCapabilitiesForModel("openai", "gpt-4o").contextWindow}`;
    const claude = `${getCapabilitiesForModel("anthropic", "claude-sonnet-4").contextWindow}`;
    expect(key === claude && key === "undefined").toBe(false);
  });
});
