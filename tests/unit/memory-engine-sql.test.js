import { describe, expect, it } from "vitest";
import { createInMemoryOps, createState, parseSelect } from "../../src/lib/db/adapters/memoryEngine.js";

// The engine interprets the SQL subset this codebase issues. Every gap in it is
// silent — an unsupported clause does not throw, it returns something plausible
// and wrong — so these pin the shapes the repos actually depend on. Two real
// bugs came out of this: LIMIT/OFFSET and MIN/MAX, both of which had been
// quietly wrong on Cloudflare while SQLite stayed correct.

const withRows = (build) => {
  const state = createState();
  const ops = createInMemoryOps(state);
  build(ops);
  return ops;
};

describe("memory engine — aggregates", () => {
  const seeded = () =>
    withRows((ops) => {
      for (const p of [3, 1, 7]) {
        ops.run(`INSERT INTO providerConnections (id, priority) VALUES (?, ?)`, [`c${p}`, p]);
      }
    });

  it("MAX with an alias", () => {
    expect(seeded().get(`SELECT MAX(priority) AS m FROM providerConnections`)).toEqual({ m: 7 });
  });

  it("MAX without an alias returns the column name", () => {
    expect(seeded().get(`SELECT MAX(priority) FROM providerConnections`)).toEqual({ priority: 7 });
  });

  it("MIN", () => {
    expect(seeded().get(`SELECT MIN(priority) AS m FROM providerConnections`)).toEqual({ m: 1 });
  });

  it("MAX respects a WHERE clause", () => {
    expect(
      seeded().get(`SELECT MAX(priority) AS m FROM providerConnections WHERE id = ?`, ["c1"])
    ).toEqual({ m: 1 });
  });

  it("MAX over nothing is null, not 0", () => {
    // 0 would be indistinguishable from a real minimum, and provider priority
    // computes (max || 0) + 1 — so a wrong 0 here silently reassigns priorities.
    expect(
      seeded().get(`SELECT MAX(priority) AS m FROM providerConnections WHERE id = ?`, ["nope"])
    ).toEqual({ m: null });
  });

  it("AVG", () => {
    const got = seeded().get(`SELECT AVG(priority) AS a FROM providerConnections`);
    expect(got.a).toBeCloseTo(11 / 3, 6);
  });

  it("COUNT and SUM still work", () => {
    const ops = seeded();
    expect(ops.get(`SELECT COUNT(*) AS n FROM providerConnections`)).toEqual({ n: 3 });
    expect(ops.get(`SELECT SUM(priority) AS s FROM providerConnections`)).toEqual({ s: 11 });
  });
});

describe("memory engine — pagination", () => {
  const five = () =>
    withRows((ops) => {
      for (let i = 1; i <= 5; i++) {
        ops.run(`INSERT INTO requestDetails (id, timestamp) VALUES (?, ?)`, [
          `r${i}`,
          `2026-01-0${i}T00:00:00Z`,
        ]);
      }
    });

  it("parses OFFSET as well as LIMIT", () => {
    const parsed = parseSelect("SELECT id FROM requestDetails ORDER BY timestamp DESC LIMIT ? OFFSET ?");
    expect(parsed.limit).toBe("?");
    expect(parsed.offset).toBe("?");
  });

  it("returns the requested page, not the whole table", () => {
    // This is the request-details tab asking for page 9999. With OFFSET
    // unrecognised the LIMIT group swallowed "LIMIT ? OFFSET ?", Number() of it
    // was NaN, and every row came back.
    const got = five().all(
      `SELECT id FROM requestDetails ORDER BY timestamp DESC LIMIT ? OFFSET ?`,
      [2, 4]
    );
    expect(got).toEqual([{ id: "r1" }]);
  });

  it("returns the first page", () => {
    expect(five().all(`SELECT id FROM requestDetails ORDER BY timestamp DESC LIMIT ?`, [2])).toEqual([
      { id: "r5" },
      { id: "r4" },
    ]);
  });

  it("returns nothing past the last page", () => {
    expect(
      five().all(`SELECT id FROM requestDetails ORDER BY timestamp DESC LIMIT ? OFFSET ?`, [20, 400])
    ).toEqual([]);
  });

  it("a WHERE placeholder does not get bound to LIMIT", () => {
    // The cursor is shared across the statement, so "WHERE provider = ? LIMIT ?"
    // must limit by the second parameter, not the provider.
    const ops = withRows((o) => {
      for (const [id, provider] of [["a", "openai"], ["b", "openai"], ["c", "anthropic"], ["d", "openai"]]) {
        o.run(`INSERT INTO requestDetails (id, provider) VALUES (?, ?)`, [id, provider]);
      }
    });
    const got = ops.all(`SELECT id FROM requestDetails WHERE provider = ? LIMIT ?`, ["openai", 2]);
    expect(got).toHaveLength(2);
    expect(got.every((r) => r.id !== "c")).toBe(true);
  });
});

describe("memory engine — SET expressions", () => {
  const seeded = () =>
    withRows((ops) => {
      ops.run(`INSERT INTO apiKeys (id, key, name, regenerateVersion, createdAt) VALUES (?, ?, ?, ?, ?)`, [
        "k1",
        "sk-1",
        "one",
        0,
        "2026-01-01",
      ]);
      ops.run(`INSERT INTO apiKeys (id, key, name, regenerateVersion, createdAt) VALUES (?, ?, ?, ?, ?)`, [
        "k2",
        "sk-2",
        "two",
        4,
        "2026-01-01",
      ]);
    });

  it("evaluates COALESCE(col, 0) + 1 against the row being updated", () => {
    // With literal-only SET parsing this stored the text
    // "COALESCE(regenerateVersion, 0) + 1" into an INTEGER column, so
    // regenerating an API key on Workers never incremented the version.
    const ops = seeded();
    ops.run(`UPDATE apiKeys SET regenerateVersion = COALESCE(regenerateVersion, 0) + 1 WHERE id = ?`, ["k1"]);
    expect(ops.get(`SELECT regenerateVersion FROM apiKeys WHERE id = ?`, ["k1"]).regenerateVersion).toBe(1);
  });

  it("each row increments from its own value, not a shared one", () => {
    const ops = seeded();
    ops.run(`UPDATE apiKeys SET regenerateVersion = COALESCE(regenerateVersion, 0) + 1`);
    expect(ops.get(`SELECT regenerateVersion FROM apiKeys WHERE id = ?`, ["k1"]).regenerateVersion).toBe(1);
    expect(ops.get(`SELECT regenerateVersion FROM apiKeys WHERE id = ?`, ["k2"]).regenerateVersion).toBe(5);
  });

  it("still handles a plain literal SET with its own placeholder", () => {
    const ops = seeded();
    ops.run(`UPDATE apiKeys SET name = ? WHERE id = ?`, ["renamed", "k2"]);
    expect(ops.get(`SELECT name FROM apiKeys WHERE id = ?`, ["k2"]).name).toBe("renamed");
    expect(ops.get(`SELECT name FROM apiKeys WHERE id = ?`, ["k1"]).name).toBe("one");
  });

  it("a SET with several assignments binds each placeholder in order", () => {
    const ops = seeded();
    ops.run(`UPDATE apiKeys SET name = ?, regenerateVersion = ? WHERE id = ?`, ["x", 9, "k1"]);
    const row = ops.get(`SELECT * FROM apiKeys WHERE id = ?`, ["k1"]);
    expect(row.name).toBe("x");
    expect(row.regenerateVersion).toBe(9);
  });
});
