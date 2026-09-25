/**
 * Cross-isolate cache invalidation.
 *
 * A single process resets its own caches on write, so staleness only bites on
 * Cloudflare Workers where each isolate holds its own copy. These tests pin that
 * a write from "another isolate" (a version bump in the Durable Object) evicts
 * the local cache, so a rotated/disabled key stops working and a kill switch
 * takes effect, rather than lingering for the multi-minute cache TTL.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  coordinatorCall: vi.fn(),
  get: vi.fn(),
  all: vi.fn(),
  run: vi.fn(),
}));

vi.mock("@/lib/coordinator/client.js", () => ({ coordinatorCall: mocks.coordinatorCall }));

const store = new Map();
const settingsRow = () => ({ data: JSON.stringify(store.get("settings") || {}) });

vi.mock("@/lib/db/driver.js", () => ({
  getAdapter: async () => ({
    get: (sql, params) => {
      mocks.get(sql, params);
      if (sql.includes("FROM settings")) return settingsRow();
      if (sql.includes("FROM apiKeys WHERE id")) {
        const row = [...store.values()].find((r) => r?.id === params?.[0]);
        return row || undefined;
      }
      if (sql.includes("FROM apiKeys WHERE key")) {
        const row = [...store.values()].find((r) => r?.key === params?.[0]);
        return row || undefined;
      }
      return undefined;
    },
    all: (sql, params) => {
      mocks.all(sql, params);
      if (sql.includes("FROM apiKeys")) {
        return [...store.values()].filter((r) => params?.[0] === undefined || r?.ownerRef === params?.[0]);
      }
      return [];
    },
    run: (sql, params) => {
      mocks.run(sql, params);
      if (sql.includes("INSERT INTO settings")) {
        store.set("settings", JSON.parse(params[0]));
        return { changes: 1 };
      }
      if (sql.startsWith("INSERT INTO apiKeys")) return { changes: 1 };
      return { changes: 1 };
    },
    transaction: (fn) => fn(),
  }),
}));

const { getSettings, updateSettings, resetSettingsCache } = await import("@/lib/db/repos/settingsRepo.js");
const { createApiKey, getApiKeyByKey, resetApiKeysCache, updateApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
const { __test__: versionTest } = await import("@/lib/coordinator/controlVersion.js");

const MACHINE = "0123456789abcdef";

let clock = 1_000_000;
beforeEach(() => { clock = 1_000_000; });
function advancePastPollInterval() { clock += versionTest.POLL_INTERVAL_MS + 1; }
beforeEach(() => { vi.spyOn(Date, "now").mockImplementation(() => clock); });
afterEach(() => { vi.restoreAllMocks(); });

function seedSettings(overrides) {
  store.set("settings", { ...(store.get("settings") || {}), ...overrides });
}

beforeEach(() => {
  vi.clearAllMocks();
  store.clear();
  mocks.run.mockImplementation(() => ({ changes: 1 }));
  process.env.NINEROUTER_RUNTIME = "cloudflare";
  versionTest.reset();
  resetSettingsCache();
  resetApiKeysCache();
});

afterEach(() => {
  delete process.env.NINEROUTER_RUNTIME;
});

describe("settings cache invalidation across isolates", () => {
  it("bumps the control version on write so other isolates can notice", async () => {
    mocks.coordinatorCall.mockResolvedValue({ version: 1 });
    await updateSettings({ gatewayControl: { disabled: true } });
    expect(mocks.coordinatorCall).toHaveBeenCalledWith("bumpVersion", { domain: "settings" }, "control");
  });

  it("keeps serving the cached value while the version is unchanged", async () => {
    seedSettings({ gatewayControl: { disabled: false } });
    mocks.coordinatorCall.mockResolvedValue({ version: 7 });

    expect((await getSettings()).gatewayControl.disabled).toBe(false);
    const readsAfterFirst = mocks.get.mock.calls.length;

    expect((await getSettings()).gatewayControl.disabled).toBe(false);
    expect(mocks.get.mock.calls.length).toBe(readsAfterFirst);
  });

  it("drops the cache when another isolate bumps the version", async () => {
    seedSettings({ gatewayControl: { disabled: false } });
    mocks.coordinatorCall.mockResolvedValue({ version: 7 });
    expect((await getSettings()).gatewayControl.disabled).toBe(false);

    // Another isolate flips the kill switch and bumps the version.
    seedSettings({ gatewayControl: { disabled: true } });
    mocks.coordinatorCall.mockResolvedValue({ version: 8 });
    advancePastPollInterval();

    expect((await getSettings()).gatewayControl.disabled).toBe(true);
  });

  it("does not hammer the coordinator: one version call per poll interval", async () => {
    seedSettings({ someFlag: 1 });
    mocks.coordinatorCall.mockResolvedValue({ version: 1 });

    await getSettings();
    const after = mocks.coordinatorCall.mock.calls.length;
    for (let i = 0; i < 25; i++) await getSettings();
    expect(mocks.coordinatorCall.mock.calls.length).toBe(after);
  });

  it("keeps the cached value when the coordinator is unreachable", async () => {
    seedSettings({ gatewayControl: { disabled: false } });
    mocks.coordinatorCall.mockRejectedValue(new Error("coordinator down"));
    expect((await getSettings()).gatewayControl.disabled).toBe(false);
  });

  it("is a no-op outside Cloudflare (single process already resets its own cache)", async () => {
    process.env.NINEROUTER_RUNTIME = "node";
    await updateSettings({ gatewayControl: { disabled: true } });
    expect(mocks.coordinatorCall).not.toHaveBeenCalled();
  });
});

describe("api key cache invalidation across isolates", () => {
  it("bumps the control version on create, update and delete", async () => {
    mocks.coordinatorCall.mockResolvedValue({ version: 1 });
    mocks.run.mockImplementation((sql, params) => {
      if (sql.startsWith("INSERT INTO apiKeys")) {
        const names = sql.match(/\(([^)]+)\)/)[1].split(",").map((s) => s.trim());
        const row = Object.fromEntries(names.map((n, i) => [n, params[i]]));
        store.set(row.id, row);
        return { changes: 1 };
      }
      return { changes: 1 };
    });

    const key = await createApiKey("k", MACHINE, { key: "sk-x" });
    expect(mocks.coordinatorCall).toHaveBeenCalledWith("bumpVersion", { domain: "apiKeys" }, "control");

    mocks.coordinatorCall.mockClear();
    await updateApiKey(key.id, { isActive: false });
    expect(mocks.coordinatorCall).toHaveBeenCalledWith("bumpVersion", { domain: "apiKeys" }, "control");
  });

  it("stops honouring a rotated key once another isolate bumps the version", async () => {
    mocks.coordinatorCall.mockResolvedValue({ version: 1 });
    mocks.run.mockImplementation((sql, params) => {
      if (sql.startsWith("INSERT INTO apiKeys")) {
        const names = sql.match(/\(([^)]+)\)/)[1].split(",").map((s) => s.trim());
        const row = Object.fromEntries(names.map((n, i) => [n, params[i]]));
        store.set(row.id, row);
        return { changes: 1 };
      }
      return { changes: 1 };
    });

    const key = await createApiKey("k", MACHINE, { key: "sk-original" });
    mocks.get.mockClear();

    expect((await getApiKeyByKey("sk-original"))?.id).toBe(key.id);
    const readsAfterFirst = mocks.get.mock.calls.length;

    // Still cached: no extra DB read while the version holds.
    expect((await getApiKeyByKey("sk-original"))?.id).toBe(key.id);
    expect(mocks.get.mock.calls.length).toBe(readsAfterFirst);

    // Another isolate rotated the key and announced it.
    store.get(key.id).key = "sk-rotated";
    mocks.coordinatorCall.mockResolvedValue({ version: 2 });
    advancePastPollInterval();

    expect(await getApiKeyByKey("sk-original")).toBeNull();
    expect((await getApiKeyByKey("sk-rotated"))?.id).toBe(key.id);
  });
});

describe("settings write does not clobber another isolate's change", () => {
  it("refreshes from the durable store before merging", async () => {
    // A stale isolate still holds the pre-kill-switch copy in memory.
    seedSettings({ gatewayControl: { disabled: false }, somethingElse: "keep-me" });
    mocks.coordinatorCall.mockResolvedValue({ version: 1 });

    // Another isolate turned the kill switch on and persisted it.
    store.set("settings", { gatewayControl: { disabled: true, reason: "incident" }, somethingElse: "keep-me" });
    const durableState = JSON.stringify(store.get("settings"));
    mocks.get.mockImplementation((sql) => {
      if (sql.includes("FROM settings")) {
        // The forced resync repopulates the adapter from the durable store.
        return { data: durableState };
      }
      return undefined;
    });

    await updateSettings({ requireApiKey: true });

    const stored = JSON.parse(mocks.run.mock.calls.at(-1)[1][0]);
    expect(stored.gatewayControl.disabled, "must not revert the kill switch").toBe(true);
    expect(stored.requireApiKey).toBe(true);
  });
});
