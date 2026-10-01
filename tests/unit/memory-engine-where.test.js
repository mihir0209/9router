import { describe, it, expect } from "vitest";
import { createInMemoryOps, buildWhere } from "../../src/lib/db/adapters/memoryEngine.js";

// These clause shapes were captured by recording every WHERE the engine actually
// executed across the whole suite. The list is the contract: a new predicate
// shape has to be added here before it is relied on in a query.
//
// That matters because the engine fails quietly. A predicate it cannot compile
// used to be treated as "matches everything", so an UPDATE or DELETE with one
// silently changed the wrong number of rows and reported success. It now throws,
// which is correct — but only useful if the set of shapes it accepts is known,
// which is what this file is.

const ROWS = [
  {
    id: "a", scope: "s1", key: "k1", provider: "openai", authType: "api", name: "n",
    isActive: 1, ownerRef: null, dateKey: "2026-01-01",
    timestamp: "2026-06-01T00:00:00.000Z", cost: 5, promptTokens: 1, completionTokens: 2,
    apiKey: "AK", apiKeyId: "id1", connectionId: "c1", model: "gpt", p: 500,
  },
  {
    id: "b", scope: "s2", key: "k2", provider: "gemini", authType: "api", name: "m",
    isActive: 0, ownerRef: "own", dateKey: "2026-02-01",
    timestamp: "2026-07-01T00:00:00.000Z", cost: 0, promptTokens: 3, completionTokens: 4,
    apiKey: "BK", apiKeyId: null, connectionId: null, model: "claude", p: 50,
  },
];

function run(clause, params = []) {
  const ops = createInMemoryOps({ tables: { t: ROWS } }, {});
  return ops.all(`SELECT id FROM t WHERE ${clause}`, params).map((r) => r.id);
}

describe("every WHERE shape the suite actually runs", () => {
  // Recorded from a full-suite run. Each must compile and evaluate.
  const SHAPES = [
    "id = ?",
    "key = ?",
    "name = ?",
    "ownerRef = ?",
    "provider = ?",
    "scope = ?",
    "dateKey = ?",
    "apiKey = ?",
    "id = 1",
    "key = 'S'",
    "scope = 'S'",
    "isActive = 0",
    "p > 100",
    "dateKey >= ?",
    "timestamp >= ?",
    "provider IS NOT NULL",
    "scope IN ('S', 'S', 'S', 'S')",
    "scope = ? AND key = ?",
    "scope = 'S' AND key = ?",
    "provider = ? AND authType = ? AND name = ?",
    "apiKeyId = ? AND provider = 'S' AND model = ?",
    "apiKey = ? AND timestamp >= ? AND cost > 0",
    "apiKey = ? AND timestamp >= ? AND timestamp <= ?",
    "timestamp >= ? AND timestamp <= ?",
    "(apiKey = ? OR (apiKeyId IS NOT NULL AND apiKeyId = ?))",
    "timestamp = ? AND COALESCE(provider, 'S') = COALESCE(?, 'S') AND COALESCE(model, 'S') = COALESCE(?, 'S') AND COALESCE(connectionId, 'S') = COALESCE(?, 'S') AND COALESCE(apiKey, 'S') = COALESCE(?, 'S') AND COALESCE(apiKeyId, 'S') = COALESCE(?, 'S') AND promptTokens = ? AND completionTokens = ?",
  ];

  it.each(SHAPES)("compiles: %s", (clause) => {
    const placeholders = (clause.match(/\?/g) || []).length;
    expect(() => run(clause, Array.from({ length: placeholders }, () => "x"))).not.toThrow();
  });
});

describe("WHERE semantics", () => {
  it("evaluates equality, ordering and null checks against real values", () => {
    expect(run("id = ?", ["a"])).toEqual(["a"]);
    expect(run("provider IS NOT NULL").sort()).toEqual(["a", "b"]);
    expect(run("ownerRef IS NULL")).toEqual(["a"]);
    expect(run("p > 100")).toEqual(["a"]);
    expect(run("timestamp >= ?", ["2026-07-01T00:00:00.000Z"])).toEqual(["b"]);
    expect(run("scope IN ('s1', 's3')")).toEqual(["a"]);
  });

  it("evaluates OR, which used to match nothing at all", () => {
    // The regression: OR was unsupported, so this compiled to one condition that
    // matched no rows and the UPDATE silently did nothing.
    expect(run("(apiKey = ? OR (apiKeyId IS NOT NULL AND apiKeyId = ?))", ["BK", "id1"]).sort())
      .toEqual(["a", "b"]);
    // row a matches on p > 100, row b on apiKey: both.
    expect(run("apiKey = ? OR p > 100", ["BK"]).sort()).toEqual(["a", "b"]);
  });

  it("evaluates COALESCE on either side", () => {
    // COALESCE(col, 'x') = COALESCE(?, 'x') has to match when the column is NULL
    // and the parameter falls back to the same literal.
    // Only row b has a NULL connectionId, so only it falls back to the literal.
    expect(run("COALESCE(connectionId, 'none') = COALESCE(?, 'none')", ["none"]))
      .toEqual(["b"]);
    expect(run("COALESCE(connectionId, 'none') = COALESCE(?, 'none')", ["c1"]))
      .toEqual(["a"]);
  });

  it("binds AND tighter than OR", () => {
    expect(run("id = 'a' OR (id = 'b' AND provider = 'gemini')").sort()).toEqual(["a", "b"]);
    expect(run("id = 'a' OR (id = 'b' AND provider = 'nope')")).toEqual(["a"]);
  });

  it("throws on a shape it cannot compile, rather than matching everything", () => {
    // Silently treating an unknown predicate as true corrupts data while
    // reporting success.
    expect(() => run("p LIKE 'x%'")).toThrow(/cannot evaluate WHERE condition/);
    expect(() => buildWhere("FUNC(a) = ?", ["x"], { value: 0 })).toThrow(/cannot evaluate WHERE condition/);
  });
});