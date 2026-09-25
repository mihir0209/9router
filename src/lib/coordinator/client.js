function memoryState() {
  if (!global._gatewayCoordinatorMemory) {
    global._gatewayCoordinatorMemory = {
      rate: new Map(),
      active: new Map(),
      locks: new Map(),
      scores: new Map(),
      budgets: new Map(),
    };
  }
  return global._gatewayCoordinatorMemory;
}

async function getStub(scope) {
  if (process.env.NINEROUTER_RUNTIME !== "cloudflare") return null;
  try {
    const { getCloudflareContext } = await import("@opennextjs/cloudflare");
    const context = await getCloudflareContext({ async: true });
    const namespace = context?.env?.GATEWAY_COORDINATOR;
    if (!namespace) return null;
    if (typeof namespace.getByName === "function") return namespace.getByName(scope || "global");
    return namespace.get(namespace.idFromName(scope || "global"));
  } catch {
    return null;
  }
}

function memoryCall(method, payload = {}) {
  const state = memoryState();
  const now = Number(payload.now) || Date.now();
  if (method === "checkRateLimit") {
    const key = `${payload.keyId}`;
    const current = state.rate.get(key);
    const active = current && now - current.windowStart < payload.windowMs ? current : { count: 0, windowStart: now };
    active.count += 1;
    state.rate.set(key, active);
    const resetAt = active.windowStart + payload.windowMs;
    return { allowed: active.count <= payload.limit, count: active.count, remaining: Math.max(payload.limit - active.count, 0), resetAt };
  }
  if (method === "beginRequest") {
    const current = state.active.get(payload.keyId) || [];
    const active = current.filter((lease) => lease.expiresAt > now);
    if (active.length >= payload.maxConcurrent) return { allowed: false, active: active.length, leaseId: null };
    const leaseId = crypto.randomUUID();
    const ttlMs = Number(payload.ttlMs) > 0 ? Number(payload.ttlMs) : 120_000;
    active.push({ leaseId, expiresAt: now + ttlMs });
    state.active.set(payload.keyId, active);
    return { allowed: true, active: active.length, leaseId };
  }
  if (method === "renewRequest") {
    const current = state.active.get(payload.keyId) || [];
    const target = current.find((lease) => lease.leaseId === payload.leaseId);
    if (!target || target.expiresAt <= now) return { renewed: false, active: current.filter((lease) => lease.expiresAt > now).length };
    target.expiresAt = now + (Number(payload.ttlMs) > 0 ? Number(payload.ttlMs) : 120_000);
    state.active.set(payload.keyId, current);
    return { renewed: true, active: current.length };
  }
  if (method === "endRequest") {
    const active = (state.active.get(payload.keyId) || []).filter((lease) => lease.leaseId !== payload.leaseId);
    state.active.set(payload.keyId, active);
    return { active: active.length };
  }
  if (method === "setLock") {
    const key = `${payload.provider}:${payload.connectionId}:${payload.model || "*"}`;
    const current = state.locks.get(key);
    const generation = Math.max(Number(current?.generation || 0), Number(state.locks.get(`gen:${payload.provider}`)?.generation || 0)) + 1;
    state.locks.set(`gen:${payload.provider}`, { generation });
    state.locks.set(key, { untilMs: payload.untilMs, status: payload.status, reason: payload.reason, generation, setAt: now });
    return { locked: true, untilMs: payload.untilMs, generation };
  }
  if (method === "clearLock") {
    const key = `${payload.provider}:${payload.connectionId}:${payload.model || "*"}`;
    const existing = state.locks.get(key);
    if (payload.expectedGeneration != null && existing && Number(existing.generation || 0) > Number(payload.expectedGeneration)) {
      return { cleared: false, reason: "newer_lock", generation: existing.generation };
    }
    state.locks.delete(key);
    return { cleared: true };
  }
  if (method === "getActiveLocks") {
    const generation = Number(state.locks.get(`gen:${payload.provider}`)?.generation) || 0;
    const locks = payload.connectionIds.flatMap((connectionId) => {
      for (const model of [payload.model, "*"].filter(Boolean)) {
        const lock = state.locks.get(`${payload.provider}:${connectionId}:${model}`);
        if (lock && lock.untilMs > now) return [{ connectionId, ...lock }];
      }
      return [];
    });
    return { locks, generation };
  }
  if (method === "recordOutcome") {
    const key = `${payload.provider}:${payload.connectionId}:${payload.model}`;
    const previous = state.scores.get(key) || { samples: 0, successes: 0, failures: 0, consecutiveFailures: 0, ewmaLatencyMs: 0, totalCostUsd: 0 };
    const latency = Number.isFinite(payload.latencyMs) ? Math.max(Number(payload.latencyMs), 0) : previous.ewmaLatencyMs;
    const entry = {
      ...previous,
      provider: payload.provider,
      connectionId: payload.connectionId,
      model: payload.model,
      samples: previous.samples + 1,
      successes: previous.successes + (payload.success ? 1 : 0),
      failures: previous.failures + (payload.success ? 0 : 1),
      consecutiveFailures: payload.success ? 0 : previous.consecutiveFailures + 1,
      ewmaLatencyMs: previous.samples === 0 ? latency : previous.ewmaLatencyMs * 0.7 + latency * 0.3,
      totalCostUsd: previous.totalCostUsd + Math.max(Number(payload.costUsd) || 0, 0),
      lastStatus: Number(payload.status) || 0,
      updatedAt: now,
    };
    state.scores.set(key, entry);
    return entry;
  }
  if (method === "getScores") {
    return payload.connectionIds.map((connectionId) => state.scores.get(`${payload.provider}:${connectionId}:${payload.model}`)).filter(Boolean);
  }
  if (method === "reserveBudget") {
    const current = (state.budgets.get(payload.keyId) || []).filter((entry) => entry.expiresAt > now);
    if (current.some((entry) => entry.requestId === payload.requestId)) return { reserved: true, amount: current.find((entry) => entry.requestId === payload.requestId).amount };
    const reservedAmount = current.reduce((total, entry) => total + Number(entry.amount || 0), 0);
    if (Number.isFinite(Number(payload.availableBudget)) && reservedAmount + Number(payload.amount) > Number(payload.availableBudget)) {
      return { reserved: false, reservedAmount };
    }
    current.push({ requestId: payload.requestId, amount: payload.amount, expiresAt: payload.expiresAt });
    state.budgets.set(payload.keyId, current);
    return { reserved: true, amount: payload.amount };
  }
  if (method === "releaseBudget") {
    state.budgets.set(payload.keyId, (state.budgets.get(payload.keyId) || []).filter((entry) => entry.requestId !== payload.requestId && entry.expiresAt > now));
    return { released: true };
  }
  if (method === "getBudgetReservations") {
    return (state.budgets.get(payload.keyId) || []).filter((entry) => entry.expiresAt > now);
  }
  if (method === "snapshot") {
    return {
      rate: [...state.rate.entries()],
      active: [...state.active.entries()],
      locks: [...state.locks.entries()],
      scores: [...state.scores.entries()],
      budgets: [...state.budgets.entries()],
    };
  }
  throw new Error(`Unsupported coordinator method: ${method}`);
}

export async function coordinatorCall(method, payload = {}, scope = "global", { failClosed = false } = {}) {
  const stub = await getStub(scope);
  if (!stub) {
    if (failClosed && process.env.NINEROUTER_RUNTIME === "cloudflare") {
      if (method === "checkRateLimit") return { allowed: false, resetAt: (Number(payload.now) || Date.now()) + Number(payload.windowMs || 60_000), unavailable: true };
      if (method === "beginRequest") return { allowed: false, unavailable: true };
      if (method === "reserveBudget") return { reserved: false, unavailable: true };
      throw new Error("Gateway coordinator is unavailable");
    }
    return memoryCall(method, payload);
  }
  try {
    if (typeof stub[method] !== "function") throw new Error("Gateway coordinator method is unavailable");
    return await stub[method](payload);
  } catch (error) {
    if (failClosed) throw error;
    return memoryCall(method, payload);
  }
}

export function scoreProviderEntries(entries, priorities = new Map()) {
  return entries
    .map((entry, index) => {
      const samples = Number(entry?.samples) || 0;
      const successes = Number(entry?.successes) || 0;
      const failures = Number(entry?.failures) || 0;
      const successRate = samples > 0 ? successes / samples : 0.5;
      const latency = Number(entry?.ewmaLatencyMs) || 0;
      const latencyScore = latency > 0 ? Math.max(0, 100 - Math.log10(Math.max(latency, 1)) * 25) : 50;
      const consecutivePenalty = Math.min(Number(entry?.consecutiveFailures) || 0, 5) * 12;
      const priority = Number(priorities.get(entry.connectionId) ?? index + 1) || index + 1;
      const cost = Number(entry?.totalCostUsd) || 0;
      const costPenalty = Math.min(cost * 0.01, 10);
      const score = samples === 0 ? 50 : Math.max(0, Math.min(100, successRate * 70 + latencyScore * 0.3 - consecutivePenalty - costPenalty));
      return { connectionId: entry.connectionId, score, samples, priority, index };
    })
    .sort((left, right) => right.score - left.score || left.priority - right.priority || left.index - right.index);
}

export async function getAdaptiveScores(provider, model, connectionIds) {
  if (!connectionIds.length) return new Map();
  const entries = await coordinatorCall("getScores", { provider, model, connectionIds }, `provider:${provider}`);
  const byConnection = new Map(entries.map((entry) => [entry.connectionId, entry]));
  return new Map(connectionIds.map((connectionId) => [connectionId, byConnection.get(connectionId) || { provider, connectionId, model, samples: 0 }]));
}

export async function recordProviderOutcome(payload) {
  return await coordinatorCall("recordOutcome", payload, `provider:${payload.provider}`);
}

export const __test__ = { memoryState, memoryCall };
