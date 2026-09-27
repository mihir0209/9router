import { describe, it, expect, beforeEach, vi } from "vitest";

/**
 * A single corrupt kv row used to crash the whole dashboard. parseJson() returns
 * null for a NULL or unparseable column, getAll() assigned it straight into the
 * result object, and ModelSelectModal then called `null.startsWith(...)` —
 * "can't access property "startsWith", fullModel is null" — taking down the
 * model picker instead of just dropping the bad row.
 *
 * The data boundary is the right place to fix this: every kv scope is read
 * through getAll(), so filtering there protects all consumers rather than the
 * handful that remember to type-check.
 */
const rows = [];
vi.mock("@/lib/db/driver.js", () => ({
  getAdapter: async () => ({
    all: (sql, params) => (sql.includes("WHERE scope") ? rows.filter((r) => r.scope === params[0]) : rows),
    run: () => {},
    ensureTables: async () => {},
  }),
}));

const model = { openai: { "gpt-4o": '"openai/gpt-4o"' }, junk: {} };

async function freshKv() {
  vi.resetModules();
  return (await import("../../src/lib/db/helpers/kvStore.js")).makeKv;
}

beforeEach(() => {
  rows.length = 0;
  rows.push(
    { scope: "modelAliases", key: "good", value: JSON.stringify("openai/gpt-4o") },
    { scope: "modelAliases", key: "nullColumn", value: null },
    { scope: "modelAliases", key: "badJson", value: "{not json" },
    { scope: "modelAliases", key: "literalNull", value: "null" },
    { scope: "customModels", key: "ok", value: JSON.stringify({ id: "x" }) },
  );
});

describe("kvStore.getAll — corrupt values", () => {
  it("drops null, unparseable and literal-null rows", async () => {
    const makeKv = await freshKv();
    const all = await makeKv("modelAliases").getAll();
    expect(all).toEqual({ good: "openai/gpt-4o" });
  });

  it("never hands back a null value to a consumer", async () => {
    const makeKv = await freshKv();
    for (const v of Object.values(await makeKv("modelAliases").getAll())) {
      expect(v).not.toBeNull();
      expect(v).not.toBeUndefined();
    }
  });

  it("is safe to feed to the .startsWith() the modal performs", async () => {
    const makeKv = await freshKv();
    const all = await makeKv("modelAliases").getAll();
    const derived = Object.entries(all)
      .filter(([, fullModel]) => typeof fullModel === "string" && fullModel.startsWith("openai/"))
      .map(([k, v]) => k);
    expect(derived).toEqual(["good"]);
  });

  it("leaves healthy scopes untouched", async () => {
    const makeKv = await freshKv();
    expect(await makeKv("customModels").getAll()).toEqual({ ok: { id: "x" } });
  });
});
