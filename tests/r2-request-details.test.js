import { describe, it, before, after, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { registerHooks } from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";

const srcRoot = path.resolve(process.cwd(), "src");
const repoRoot = process.cwd();
registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier.startsWith("@/")) {
      const target = pathToFileURL(path.join(srcRoot, specifier.slice(2))).href;
      return nextResolve(target, context);
    }
    if (specifier.startsWith("open-sse/")) {
      const target = pathToFileURL(path.join(repoRoot, specifier)).href;
      return nextResolve(target, context);
    }
    return nextResolve(specifier, context);
  },
});

import {
  buildR2DetailKey,
  shouldOffloadToR2,
  buildCompactD1Record,
  offloadRequestDetailToR2,
  hydrateRequestDetailFromR2,
  hydrateRequestDetailsFromR2,
  setR2Bucket,
} from "../src/cloudflare/r2-details.js";

function createMockR2Bucket() {
  const store = new Map();
  return {
    store,
    async put(key, value, opts) {
      const text = typeof value === "string" ? value : JSON.stringify(value);
      store.set(key, { text, opts });
    },
    async get(key) {
      const entry = store.get(key);
      if (!entry) return null;
      return {
        async text() {
          return entry.text;
        },
        async json() {
          return JSON.parse(entry.text);
        },
      };
    },
    async delete(key) {
      store.delete(key);
    },
  };
}

function makeHeavyDetail(id, sizeBytes = 20 * 1024) {
  const chunk = "x".repeat(Math.max(100, Math.floor(sizeBytes / 2)));
  return {
    id,
    provider: "anthropic",
    model: "claude-sonnet-4-5",
    connectionId: "conn-1",
    timestamp: "2026-11-25T10:30:00.000Z",
    status: "ok",
    latency: { ttft: 120, total: 850 },
    tokens: { input: 2400, output: 600 },
    request: {
      headers: { authorization: "Bearer secret-token", "content-type": "application/json" },
      messages: [
        { role: "user", content: chunk },
        { role: "assistant", content: "ack" },
        { role: "user", content: "follow-up" },
      ],
    },
    providerRequest: { body: { model: "claude-sonnet-4-5", prompt: chunk.slice(0, 500) } },
    providerResponse: { id: "msg_123", usage: { input_tokens: 2400, output_tokens: 600 } },
    response: {
      choices: [{ message: { role: "assistant", content: chunk } }],
    },
  };
}

describe("r2-details unit helpers", () => {
  beforeEach(() => {
    setR2Bucket(null);
  });

  it("buildR2DetailKey formats request-details/<YYYY-MM-DD>/<id>.json", () => {
    const key = buildR2DetailKey("req-abc-123", "2026-11-25T10:30:00.000Z");
    assert.equal(key, "request-details/2026-11-25/req-abc-123.json");
  });

  it("shouldOffloadToR2 returns false for tiny metadata-only records and true for body-bearing or large records", () => {
    assert.equal(shouldOffloadToR2({ id: "small-1", provider: "openai", request: {}, response: {} }, 1024), false);
    assert.equal(shouldOffloadToR2({ id: "body-1", request: { messages: [{ role: "user", content: "hi" }] } }, 1024), true);
    assert.equal(shouldOffloadToR2({ id: "big-1", extra: "a".repeat(2048) }, 1024), true);
  });

  it("offloadRequestDetailToR2 stores full payload in R2 and strips heavy blobs from D1 record", async () => {
    const bucket = createMockR2Bucket();
    const full = makeHeavyDetail("heavy-1", 20 * 1024);

    const res = await offloadRequestDetailToR2(full, { bucket });
    assert.equal(res.offloaded, true);
    assert.equal(res.r2Key, "request-details/2026-11-25/heavy-1.json");
    assert.ok(bucket.store.has(res.r2Key));

    // Compact D1 record must not carry heavy request/response blobs
    assert.equal(res.record.request, undefined);
    assert.equal(res.record.providerRequest, undefined);
    assert.equal(res.record.providerResponse, undefined);
    assert.equal(res.record.response, undefined);
    assert.equal(res.record.r2Key, res.r2Key);
    assert.equal(res.record.provider, "anthropic");
    assert.equal(res.record.model, "claude-sonnet-4-5");
    assert.ok(res.compactBytes < 512, `Expected compactBytes < 512, got ${res.compactBytes}`);
    assert.ok(res.originalBytes > 20 * 1024);
  });

  it("hydrateRequestDetailFromR2 restores full payload and falls back gracefully when missing or unavailable", async () => {
    const bucket = createMockR2Bucket();
    const full = makeHeavyDetail("hyd-1", 8 * 1024);
    const { record: compact } = await offloadRequestDetailToR2(full, { bucket });

    // Hydrate with bucket available
    const hydrated = await hydrateRequestDetailFromR2(compact, { bucket });
    assert.equal(hydrated.request.messages.length, 3);
    assert.equal(hydrated.response.choices[0].message.content, full.response.choices[0].message.content);
    assert.equal(hydrated.r2Key, compact.r2Key);

    // Graceful fallback when object is missing in R2
    await bucket.delete(compact.r2Key);
    const missingFallback = await hydrateRequestDetailFromR2(compact, { bucket });
    assert.deepEqual(missingFallback, compact);

    // Graceful fallback when R2 bucket is unavailable (e.g. local SQLite/Postgres)
    const noBucketFallback = await hydrateRequestDetailFromR2(compact, {});
    assert.deepEqual(noBucketFallback, compact);
  });

  it("resolveR2Bucket via globalThis cloudflare context symbol", async () => {
    const bucket = createMockR2Bucket();
    const sym = Symbol.for("__cloudflare-context__");
    const prev = globalThis[sym];
    globalThis[sym] = { env: { REQUEST_DETAILS_BUCKET: bucket } };
    try {
      const full = makeHeavyDetail("ctx-1", 4096);
      const res = await offloadRequestDetailToR2(full);
      assert.equal(res.offloaded, true);
      const hydrated = await hydrateRequestDetailFromR2(res.record);
      assert.equal(hydrated.response.choices[0].message.content, full.response.choices[0].message.content);
    } finally {
      if (prev === undefined) delete globalThis[sym];
      else globalThis[sym] = prev;
    }
  });
});

describe("requestDetailsRepo + R2 integration", () => {
  const prevEnableLogs = process.env.ENABLE_REQUEST_LOGS;
  const prevDbAdapter = global._dbAdapter;
  let sqliteDb;
  let repo;
  let adapter;

  before(async () => {
    process.env.ENABLE_REQUEST_LOGS = "true";
    sqliteDb = new DatabaseSync(":memory:");
    sqliteDb.exec(`
      CREATE TABLE IF NOT EXISTS settings (
        id INTEGER PRIMARY KEY,
        data TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS requestDetails (
        id TEXT PRIMARY KEY,
        timestamp TEXT NOT NULL,
        provider TEXT,
        model TEXT,
        connectionId TEXT,
        status TEXT,
        data TEXT NOT NULL
      )
    `);
    adapter = {
      driver: "node:sqlite-memory-test",
      exec: (sql) => sqliteDb.exec(sql),
      run: (sql, params = []) => sqliteDb.prepare(sql).run(...params),
      get: (sql, params = []) => sqliteDb.prepare(sql).get(...params),
      all: (sql, params = []) => sqliteDb.prepare(sql).all(...params),
      transaction: (fn) => {
        sqliteDb.exec("BEGIN");
        try {
          const out = fn();
          sqliteDb.exec("COMMIT");
          return out;
        } catch (err) {
          sqliteDb.exec("ROLLBACK");
          throw err;
        }
      },
    };
    global._dbAdapter = { instance: adapter, initPromise: null, logged: true };
    repo = await import("../src/lib/db/repos/requestDetailsRepo.js");
  });

  after(() => {
    setR2Bucket(null);
    sqliteDb?.close();
    if (prevDbAdapter === undefined) delete global._dbAdapter;
    else global._dbAdapter = prevDbAdapter;
    if (prevEnableLogs === undefined) delete process.env.ENABLE_REQUEST_LOGS;
    else process.env.ENABLE_REQUEST_LOGS = prevEnableLogs;
  });

  beforeEach(() => {
    setR2Bucket(null);
  });

  it("saveRequestDetail offloads heavy payload to R2, stores compact row in DB, and hydrates on getRequestDetailById & getRequestDetails", async () => {
    const bucket = createMockR2Bucket();
    setR2Bucket(bucket);

    const detail = makeHeavyDetail("repo-r2-1", 20 * 1024);
    await repo.saveRequestDetail(detail);
    await repo.flushRequestDetailsBuffer();

    // Verify DB row is compact and stripped of heavy blobs
    const rawRow = adapter.get(`SELECT data FROM requestDetails WHERE id = ?`, ["repo-r2-1"]);
    assert.ok(rawRow, "Row should exist in DB");
    const stored = JSON.parse(rawRow.data);
    assert.equal(stored.r2Key, "request-details/2026-11-25/repo-r2-1.json");
    assert.equal(stored.request, undefined);
    assert.equal(stored.response, undefined);
    assert.ok(rawRow.data.length < 512, `DB data column should be compact, got ${rawRow.data.length} bytes`);

    // Verify getRequestDetailById transparently hydrates from R2 (and sanitized authorization header)
    const byId = await repo.getRequestDetailById("repo-r2-1");
    assert.ok(byId);
    assert.equal(byId.request.headers.authorization, undefined);
    assert.equal(byId.request.headers["content-type"], "application/json");
    assert.equal(byId.request.messages.length, 3);
    assert.equal(byId.response.choices[0].message.content.length, 10240);

    // Verify getRequestDetails transparently hydrates from R2
    const list = await repo.getRequestDetails({ provider: "anthropic" });
    const found = list.details.find((d) => d.id === "repo-r2-1");
    assert.ok(found);
    assert.equal(found.response.choices[0].message.content.length, 10240);

    // Verify graceful fallback when R2 is unavailable on read
    setR2Bucket(null);
    const fallback = await repo.getRequestDetailById("repo-r2-1");
    assert.equal(fallback.id, "repo-r2-1");
    assert.equal(fallback.r2Key, "request-details/2026-11-25/repo-r2-1.json");
  });

  it("falls back to inline truncated DB write when R2 put throws", async () => {
    setR2Bucket({
      async put() {
        throw new Error("R2 transient outage");
      },
      async get() {
        return null;
      },
    });

    const detail = makeHeavyDetail("repo-r2-err-1", 10 * 1024);
    await repo.saveRequestDetail(detail);
    await repo.flushRequestDetailsBuffer();

    const byId = await repo.getRequestDetailById("repo-r2-err-1");
    assert.ok(byId);
    assert.equal(byId.id, "repo-r2-err-1");
    assert.equal(byId.r2Key, undefined);
    assert.equal(byId.request._truncated, true);
  });
});
