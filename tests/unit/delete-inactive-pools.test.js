import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeAll, afterAll, vi } from "vitest";

const originalDataDir = process.env.DATA_DIR;
let tempDir;
let db;

beforeAll(async () => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-delete-inactive-"));
  process.env.DATA_DIR = tempDir;
  vi.resetModules();
  db = await import("@/lib/db/index.js");
  await db.initDb();
});

afterAll(() => {
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
});

async function seedPool(id, isActive) {
  return db.createProxyPool({
    id,
    name: `pool-${id}`,
    proxyUrl: `http://${id}:8080`,
    isActive,
  });
}

describe("deleteInactiveProxyPools", () => {
  it("deletes only inactive pools and protects bound ids", async () => {
    await seedPool("active-1", true);
    await seedPool("inactive-1", false);
    await seedPool("inactive-2", false);
    await seedPool("inactive-bound", false);

    const result = await db.deleteInactiveProxyPools(["inactive-bound"]);

    expect(result.deletedCount).toBe(2);
    expect(result.deletedIds.sort()).toEqual(["inactive-1", "inactive-2"]);
    expect(result.skippedIds).toEqual(["inactive-bound"]);

    const remaining = await db.getProxyPools();
    const remainingIds = remaining.map((p) => p.id).sort();
    expect(remainingIds).toEqual(["active-1", "inactive-bound"]);
  });

  it("deletes everything inactive when nothing is protected", async () => {
    await seedPool("inactive-3", false);

    const result = await db.deleteInactiveProxyPools([]);

    expect(result.deletedCount).toBe(2);
    expect(result.deletedIds.sort()).toEqual(["inactive-3", "inactive-bound"]);
    const remaining = await db.getProxyPools();
    expect(remaining.map((p) => p.id).sort()).toEqual(["active-1"]);
  });

  it("is a no-op with no inactive pools", async () => {
    const before = await db.getProxyPools();
    const result = await db.deleteInactiveProxyPools([]);
    const after = await db.getProxyPools();

    expect(result.deletedCount).toBe(0);
    expect(after.length).toBe(before.length);
  });

  it("tolerates non-array protectedIds", async () => {
    await seedPool("inactive-x", false);
    const result = await db.deleteInactiveProxyPools(undefined);
    expect(result.deletedCount).toBe(1);
    expect(await db.getProxyPoolById("inactive-x")).toBeNull();
  });
});
