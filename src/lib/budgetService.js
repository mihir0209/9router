import { getAdapter } from "@/lib/db/driver.js";
import { getApiKeyByKey } from "@/lib/db/repos/apiKeysRepo.js";
import { saveRequestUsage } from "@/lib/db/repos/usageRepo.js";
import { coordinatorCall } from "@/lib/coordinator/client.js";

const locks = new Map();
const reservations = new Map();

function getWindow(keyRecord, now) {
  const resetHours = Number(keyRecord.budgetResetHours);
  const anchor = new Date(keyRecord.budgetWindowStartedAt || keyRecord.createdAt || 0);
  const anchorMs = Number.isFinite(anchor.getTime()) ? anchor.getTime() : 0;
  if (!Number.isFinite(resetHours) || resetHours <= 0) {
    return { startsAt: new Date(anchorMs), resetsAt: null };
  }

  const windowMs = resetHours * 60 * 60 * 1000;
  const elapsedWindows = Math.max(Math.floor((now.getTime() - anchorMs) / windowMs), 0);
  const startsAt = new Date(anchorMs + (elapsedWindows * windowMs));
  return { startsAt, resetsAt: new Date(startsAt.getTime() + windowMs) };
}

async function withKeyLock(apiKey, operation) {
  const previous = locks.get(apiKey) || Promise.resolve();
  let release;
  const current = new Promise((resolve) => { release = resolve; });
  locks.set(apiKey, current);
  await previous;
  try {
    return await operation();
  } finally {
    release();
    if (locks.get(apiKey) === current) locks.delete(apiKey);
  }
}

async function readBudgetState(keyRecord, now) {
  const limit = Number(keyRecord?.budgetUsd);
  if (!keyRecord || keyRecord.budgetUsd == null || !Number.isFinite(limit) || limit < 0) {
    return { enabled: false, limit: 0, used: 0, reserved: 0, remaining: null, exceeded: false, resetsAt: null };
  }

  const { startsAt, resetsAt } = getWindow(keyRecord, now);
  const db = await getAdapter();
  await db.ensureTables?.(["usageHistory"]);
  const byKey = db.get(
    `SELECT COALESCE(SUM(cost), 0) AS used
       FROM usageHistory
      WHERE apiKey = ? AND timestamp >= ? AND cost > 0`,
    [keyRecord.key, startsAt.toISOString()]
  );
  let used = Number(byKey?.used) || 0;
  if (keyRecord.regenerateVersion > 0) {
    const historical = db.get(
      `SELECT COALESCE(SUM(cost), 0) AS used
         FROM usageHistory
        WHERE apiKeyId = ? AND apiKey != ? AND timestamp >= ? AND cost > 0`,
      [keyRecord.id, keyRecord.key, startsAt.toISOString()]
    );
    used += Number(historical?.used) || 0;
  }
  let reserved = 0;
  if (process.env.NINEROUTER_RUNTIME === "cloudflare") {
    const durable = await coordinatorCall("getBudgetReservations", { keyId: keyRecord.id, now: now.getTime() }, `tenant:${keyRecord.ownerRef || keyRecord.id}`, { failClosed: true });
    reserved = (durable || []).reduce((total, entry) => total + Number(entry.amount || 0), 0);
  } else {
    for (const [key, entry] of reservations) {
      if (entry.expiresAt <= now.getTime()) reservations.delete(key);
    }
    reserved = [...reservations.values()]
      .filter((entry) => entry.apiKey === keyRecord.key)
      .reduce((total, entry) => total + entry.amount, 0);
  }
  const remaining = Math.max(limit - used - reserved, 0);

  return {
    enabled: true,
    limit,
    used,
    reserved,
    remaining,
    exceeded: used + reserved >= limit,
    resetsAt: resetsAt?.toISOString() || null,
  };
}

export async function getBudgetState(apiKey, now = new Date()) {
  if (!apiKey) return { enabled: false, limit: 0, used: 0, reserved: 0, remaining: null, exceeded: false, resetsAt: null };
  return withKeyLock(apiKey, async () => {
    const keyRecord = await getApiKeyByKey(apiKey);
    return readBudgetState(keyRecord, now);
  });
}

export async function checkBudget(apiKey, now = new Date()) {
  return getBudgetState(apiKey, now);
}

export async function debitBudget(apiKey, { requestId, reserveUsd, commitUsd }, options = {}) {
  if (typeof requestId !== "string" || !requestId.trim()) throw new TypeError("requestId is required");
  const hasReserve = reserveUsd !== undefined;
  const hasCommit = commitUsd !== undefined;
  if (hasReserve === hasCommit) throw new TypeError("Provide exactly one of reserveUsd or commitUsd");

  const amount = Number(hasReserve ? reserveUsd : commitUsd);
  if (!Number.isFinite(amount) || amount < 0 || (hasReserve && amount === 0)) {
    throw new TypeError(hasReserve ? "reserveUsd must be positive" : "commitUsd must be non-negative");
  }

  return withKeyLock(apiKey, async () => {
    const keyRecord = await getApiKeyByKey(apiKey);
    if (!keyRecord) throw new Error("API key not found");

    const now = options.now ? new Date(options.now) : new Date();
    const state = await readBudgetState(keyRecord, now);
    if (!state.enabled) throw new Error("API key does not have a budget");

    const reservationKey = `${apiKey}:${requestId}`;
    if (hasReserve) {
      if (amount > state.remaining) return { ...state, reserved: false };
      if (process.env.NINEROUTER_RUNTIME === "cloudflare") {
        const result = await coordinatorCall("reserveBudget", { keyId: keyRecord.id, requestId, amount, availableBudget: state.remaining + state.reserved, expiresAt: now.getTime() + 60 * 60 * 1000, now: now.getTime() }, `tenant:${keyRecord.ownerRef || keyRecord.id}`, { failClosed: true });
        if (!result?.reserved) return { ...state, reserved: false };
      } else {
        const existing = reservations.get(reservationKey);
        if (existing) return { ...state, reserved: true };
        reservations.set(reservationKey, { apiKey, amount, expiresAt: now.getTime() + 60 * 60 * 1000 });
      }
      return { ...await readBudgetState(keyRecord, now), reserved: true };
    }

    const db = await getAdapter();
    await db.ensureTables?.(["usageHistory"]);
    const model = `budget-commit:${requestId}`;
    const existingCommit = db.get(
      `SELECT cost FROM usageHistory WHERE apiKeyId = ? AND provider = 'budget' AND model = ? LIMIT 1`,
      [keyRecord.id, model]
    );
    if (existingCommit) {
      reservations.delete(reservationKey);
      if (process.env.NINEROUTER_RUNTIME === "cloudflare") {
        await coordinatorCall("releaseBudget", { keyId: keyRecord.id, requestId, now: now.getTime() }, `tenant:${keyRecord.ownerRef || keyRecord.id}`);
      }
      return { ...await readBudgetState(keyRecord, now), committed: true };
    }

    let reservation = reservations.get(reservationKey);
    if (process.env.NINEROUTER_RUNTIME === "cloudflare") {
      const durableReservations = await coordinatorCall("getBudgetReservations", { keyId: keyRecord.id, now: now.getTime() }, `tenant:${keyRecord.ownerRef || keyRecord.id}`, { failClosed: true });
      reservation = durableReservations.find((entry) => entry.requestId === requestId) || null;
    } else {
      reservations.delete(reservationKey);
    }
    const stateWithoutReservation = process.env.NINEROUTER_RUNTIME === "cloudflare"
      ? { ...state, remaining: state.remaining + Number(reservation?.amount || 0) }
      : await readBudgetState(keyRecord, now);
    if (!reservation && amount > stateWithoutReservation.remaining) {
      return { ...stateWithoutReservation, committed: false };
    }

    await saveRequestUsage({
      timestamp: now.toISOString(),
      provider: "budget",
      model,
      apiKey,
      apiKeyId: keyRecord.id,
      endpoint: "/api/key-actions/debit",
      status: "success",
      tokens: {},
      cost: amount,
      // Ledger entry, not analytics: must survive extreme write mode, else
      // budget accounting resets on process/isolate recycle (overspend risk).
      forcePersist: true,
    });

    return { ...await readBudgetState(keyRecord, now), committed: true };
  });
}

export function budgetHeaders(state) {
  if (!state?.enabled) return {};
  const headers = {
    "X-Budget-Limit": String(state.limit),
    "X-Budget-Used": String(state.used),
    "X-Budget-Remaining": String(state.remaining),
  };
  if (state.resetsAt) headers["X-Budget-Reset"] = state.resetsAt;
  return headers;
}

export const __test__ = {
  clearReservations() {
    reservations.clear();
  },
};
