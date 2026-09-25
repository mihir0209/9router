/**
 * API-key update semantics.
 *
 * `updateApiKey` used to rebuild the whole row from a pre-read snapshot, so a
 * policy update landing after a rotation wrote the pre-rotation key back and
 * silently un-did it. It must now write only the supplied columns and report
 * the row as it actually stands.
 */
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const originalDataDir = process.env.DATA_DIR;
let tempDir;
let db;

beforeAll(async () => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-keyupdate-"));
  process.env.DATA_DIR = tempDir;
  vi.resetModules();
  db = await import("@/lib/db/index.js");
  await db.initDb();
});

afterAll(() => {
  try { global._dbAdapter?.instance?.close?.(); } catch {}
  delete global._dbAdapter;
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
});

const MACHINE = "0123456789abcdef";

async function makeKey(overrides = {}, name = "k") {
  return await db.createApiKey(name, MACHINE, { key: `sk-${Math.random().toString(36).slice(2, 12)}`, ...overrides });
}

describe("updateApiKey — partial update", () => {
  let key;
  beforeEach(() => {
    key = null;
  });

  it("only writes the supplied columns", async () => {
    key = await makeKey({ description: "before", rateLimitRequests: 5 }, "original");
    await db.updateApiKey(key.id, { description: "after" });

    const stored = await db.getApiKeyById(key.id);
    expect(stored.description).toBe("after");
    expect(stored.name).toBe("original");
    expect(stored.rateLimitRequests).toBe(5);
  });

  it("does not resurrect a key that was rotated after the read", async () => {
    key = await makeKey({ name: "rotate-me" });
    const originalKey = key.key;

    const rotated = await db.regenerateApiKey(key.id);
    expect(rotated.key).not.toBe(originalKey);

    // A concurrent policy update must not write the stale key back.
    const updated = await db.updateApiKey(key.id, { description: "policy tweak" });
    expect(updated.key).toBe(rotated.key);
    expect((await db.getApiKeyById(key.id)).key).toBe(rotated.key);
    // The rotated secret no longer authenticates.
    expect(await db.validateApiKey(originalKey)).toBeFalsy();
    expect(await db.validateApiKey(rotated.key)).toBeTruthy();
  });

  it("returns the real post-update row, not a guess", async () => {
    key = await makeKey({ ownerRef: "acme", maxConcurrentRequests: 1 });
    // Someone else bumps the budget between our read and our write.
    await db.updateApiKey(key.id, { budgetUsd: 42 });

    const updated = await db.updateApiKey(key.id, { description: "x" });
    expect(updated.budgetUsd).toBe(42);
    expect(updated.description).toBe("x");
    expect(updated.ownerRef).toBe("acme");
  });

  it("normalises isActive and metadata like a full write did", async () => {
    key = await makeKey();
    const disabled = await db.updateApiKey(key.id, { isActive: false, metadata: { team: "core" } });
    expect(disabled.isActive).toBe(false);
    expect(disabled.metadata).toEqual({ team: "core" });
    expect((await db.getApiKeyById(key.id)).isActive).toBe(false);
  });

  it("ignores unknown fields instead of writing them", async () => {
    key = await makeKey({}, "keep");
    const updated = await db.updateApiKey(key.id, { description: "ok", bogusField: "nope" });
    expect(updated.description).toBe("ok");
    expect(updated.name).toBe("keep");
    expect(updated.bogusField).toBeUndefined();
  });

  it("treats a no-op key update as a touch, not a write of the old key", async () => {
    key = await makeKey();
    const rotated = await db.regenerateApiKey(key.id);
    // Caller echoes the current key back (e.g. a form that submits the secret).
    const updated = await db.updateApiKey(rotated.id, { key: rotated.key, description: "touched" });
    expect(updated.key).toBe(rotated.key);
    expect(updated.description).toBe("touched");
  });

  it("still rejects a duplicate key", async () => {
    const a = await makeKey();
    const b = await makeKey();
    await expect(db.updateApiKey(b.id, { key: a.key })).rejects.toMatchObject({ code: "DUPLICATE_API_KEY" });
  });

  it("returns null for a missing key", async () => {
    expect(await db.updateApiKey("does-not-exist", { description: "x" })).toBeNull();
  });
});
