import { DurableObject } from "cloudflare:workers";
import { EdgeWasm } from "./edge-wasm.js";

let registeredWasmModule = null;

/**
 * Registers a pre-compiled WebAssembly.Module (from a Wrangler CompiledWasm
 * ES import) so every GatewayCoordinator Durable Object instantiates EdgeWasm
 * synchronously with zero filesystem access.
 * @param {WebAssembly.Module} wasmModule
 */
export function setEdgeWasmModule(wasmModule) {
  if (wasmModule instanceof WebAssembly.Module) {
    registeredWasmModule = wasmModule;
    globalThis.__9ROUTER_EDGE_WASM_MODULE__ = wasmModule;
  }
}

export function getEdgeWasmModule() {
  if (registeredWasmModule instanceof WebAssembly.Module) return registeredWasmModule;
  const globalMod = globalThis.__9ROUTER_EDGE_WASM_MODULE__;
  return globalMod instanceof WebAssembly.Module ? globalMod : null;
}

const MAX_FAST_U31 = 0x7fffffff;
const MAX_FAST_U32 = 0xffffffff;
const SCORE_TTL_MS = 86_400_000;

function mapScoreEntry(raw) {
  if (!raw) return null;
  return {
    provider: raw.provider,
    connectionId: raw.connection_id ?? raw.connectionId,
    model: raw.model,
    samples: Number(raw.samples) || 0,
    successes: Number(raw.successes) || 0,
    failures: Number(raw.failures) || 0,
    consecutiveFailures: Number(raw.consecutive_failures ?? raw.consecutiveFailures) || 0,
    ewmaLatencyMs: Number(raw.ewma_latency_ms ?? raw.ewmaLatencyMs) || 0,
    totalCostUsd: Number(raw.total_cost_usd ?? raw.totalCostUsd) || 0,
    lastStatus: Number(raw.last_status ?? raw.lastStatus) || 0,
    updatedAt: Number(raw.updated_at ?? raw.updatedAt) || 0,
  };
}

export class GatewayCoordinator extends DurableObject {
  constructor(ctx, env, options = {}) {
    super(ctx, env);
    this.env = env;
    this.ctx = ctx;

    this._wasm = null;
    this._wasmPromise = null;
    this._wasmDisabled = options?.wasm === false;

    if (options?.wasm instanceof EdgeWasm) {
      this._wasm = options.wasm;
    } else if (!this._wasmDisabled) {
      const mod = options?.wasmModule || env?.EDGE_WASM_MODULE || getEdgeWasmModule();
      if (mod instanceof WebAssembly.Module) {
        try {
          this._wasm = EdgeWasm.fromModule(mod);
        } catch {
          this._wasmDisabled = true;
        }
      }
    }

    this._hydratedRates = new Set();
    this._rateColdCarry = new Map();
    this._hydratedLeases = new Set();
    this._legacyLeases = new Map();
    this._wasmLeases = new Map();
    this._hydratedLockProviders = new Set();
    this._hydratedLockKeys = new Set();
    this._lockGenBase = new Map();
    this._legacyLocks = new Map();
    this._lockMirrors = new Map();
    this._hydratedScores = new Set();
    this._presentScores = new Set();
    this._hydratedBudgets = new Set();
    this._budgetMirrors = new Map();
    this._hydratedVersions = new Set();
    this._routingConfig = null;
    this._rrCounters = new Map();
    this._stickyPicks = new Map();
  }

  async _getWasm() {
    if (this._wasmDisabled) return null;
    if (this._wasm) return this._wasm;
    const mod = getEdgeWasmModule();
    if (mod instanceof WebAssembly.Module) {
      try {
        this._wasm = EdgeWasm.fromModule(mod);
        if (this._routingConfig) this._wasm.configureRouting(this._routingConfig);
        return this._wasm;
      } catch {
        this._wasmDisabled = true;
        return null;
      }
    }
    if (!this._wasmPromise) {
      this._wasmPromise = EdgeWasm.load()
        .then((inst) => {
          this._wasm = inst;
          if (this._routingConfig) this._wasm.configureRouting(this._routingConfig);
          return inst;
        })
        .catch(() => {
          this._wasmDisabled = true;
          return null;
        });
    }
    return await this._wasmPromise;
  }

  async getEdgeWasmStatus() {
    const wasm = await this._getWasm();
    return {
      enabled: Boolean(wasm),
      disabled: this._wasmDisabled,
      memoryBytes: wasm?.memory?.buffer?.byteLength || 0,
      memoryPages: wasm ? wasm.memory.buffer.byteLength / 65536 : 0,
    };
  }

  async checkRateLimit({ keyId, limit, windowMs, now = Date.now() }) {
    const key = `rate:${keyId}`;
    const wasm = await this._getWasm();
    if (!wasm) {
      return await this._checkRateLimitJs({ key, limit, windowMs, now });
    }

    if (!this._hydratedRates.has(keyId)) {
      const stored = await this.ctx.storage.get(key);
      this._hydratedRates.add(keyId);
      if (stored && now - stored.windowStart < windowMs && Number(stored.count) > 0) {
        this._rateColdCarry.set(keyId, { count: Number(stored.count), windowStart: stored.windowStart });
      }
    }

    const carry = this._rateColdCarry.get(keyId);
    if (carry) {
      if (now - carry.windowStart < windowMs) {
        carry.count += 1;
        const resetAt = carry.windowStart + windowMs;
        await this.ctx.storage.put(key, { count: carry.count, windowStart: carry.windowStart });
        return {
          allowed: carry.count <= limit,
          count: carry.count,
          remaining: Math.max(limit - carry.count, 0),
          resetAt,
        };
      }
      this._rateColdCarry.delete(keyId);
    }

    let allowed;
    let count;
    let remaining;
    let resetAt;
    if (limit >= 0 && limit <= MAX_FAST_U31 && windowMs > 0 && windowMs <= MAX_FAST_U32) {
      const fast = wasm.fastCheckRateLimit(keyId, limit, windowMs, now);
      allowed = fast.allowed;
      remaining = fast.remaining;
      resetAt = fast.resetAt;
      if (allowed) {
        count = limit - remaining;
      } else {
        const peek = wasm.peekRateLimit({ keyId, limit, windowMs, now });
        count = peek.count;
        resetAt = peek.reset_at ?? resetAt;
      }
    } else {
      const res = wasm.checkRateLimit({ keyId, limit, windowMs, now });
      allowed = res.allowed;
      count = res.count;
      remaining = res.remaining;
      resetAt = res.reset_at;
    }

    await this.ctx.storage.put(key, { count, windowStart: resetAt - windowMs });
    return { allowed, count, remaining, resetAt };
  }

  async _checkRateLimitJs({ key, limit, windowMs, now }) {
    const current = await this.ctx.storage.get(key);
    const active = current && now - current.windowStart < windowMs ? current : { count: 0, windowStart: now };
    active.count += 1;
    const resetAt = active.windowStart + windowMs;
    await this.ctx.storage.put(key, active);
    return {
      allowed: active.count <= limit,
      count: active.count,
      remaining: Math.max(limit - active.count, 0),
      resetAt,
    };
  }

  // Read-only twin of checkRateLimit for dashboards: same window math, no
  // increment, so polling "remaining" never burns budget.
  async peekRateLimit({ keyId, limit, windowMs, now = Date.now() }) {
    const key = `rate:${keyId}`;
    const wasm = await this._getWasm();
    if (!wasm) {
      const current = await this.ctx.storage.get(key);
      const active = current && now - current.windowStart < windowMs ? current : { count: 0, windowStart: now };
      return {
        count: active.count,
        remaining: Math.max(limit - active.count, 0),
        resetAt: active.windowStart + windowMs,
      };
    }

    if (!this._hydratedRates.has(keyId)) {
      const stored = await this.ctx.storage.get(key);
      this._hydratedRates.add(keyId);
      if (stored && now - stored.windowStart < windowMs && Number(stored.count) > 0) {
        this._rateColdCarry.set(keyId, { count: Number(stored.count), windowStart: stored.windowStart });
      }
    }

    const carry = this._rateColdCarry.get(keyId);
    if (carry) {
      if (now - carry.windowStart < windowMs) {
        return {
          count: carry.count,
          remaining: Math.max(limit - carry.count, 0),
          resetAt: carry.windowStart + windowMs,
        };
      }
      this._rateColdCarry.delete(keyId);
    }

    const peek = wasm.peekRateLimit({ keyId, limit, windowMs, now });
    return {
      count: peek.count,
      remaining: peek.remaining,
      resetAt: peek.reset_at,
    };
  }

  async _hydrateLeases(keyId, now) {
    if (this._hydratedLeases.has(keyId)) return;
    const stored = await this.ctx.storage.get(`active:${keyId}`);
    this._hydratedLeases.add(keyId);
    if (Array.isArray(stored?.leases) && stored.leases.length) {
      const active = stored.leases.filter((lease) => lease.expiresAt > now);
      if (active.length) this._legacyLeases.set(keyId, active);
    }
  }

  _pruneLeases(keyId, now) {
    const legacy = (this._legacyLeases.get(keyId) || []).filter((lease) => lease.expiresAt > now);
    if (legacy.length) this._legacyLeases.set(keyId, legacy);
    else this._legacyLeases.delete(keyId);

    const wasmList = (this._wasmLeases.get(keyId) || []).filter((lease) => lease.expiresAt > now);
    if (wasmList.length) this._wasmLeases.set(keyId, wasmList);
    else this._wasmLeases.delete(keyId);

    return { legacy, wasmList };
  }

  async beginRequest({ keyId, maxConcurrent, now = Date.now() }) {
    const key = `active:${keyId}`;
    const wasm = await this._getWasm();
    if (!wasm) {
      const current = await this.ctx.storage.get(key);
      const active = Array.isArray(current?.leases) ? current.leases.filter((lease) => lease.expiresAt > now) : [];
      if (active.length >= maxConcurrent) {
        await this.ctx.storage.put(key, { leases: active });
        return { allowed: false, active: active.length, leaseId: null };
      }
      const leaseId = crypto.randomUUID();
      active.push({ leaseId, expiresAt: now + 120_000 });
      await this.ctx.storage.put(key, { leases: active });
      return { allowed: true, active: active.length, leaseId };
    }

    await this._hydrateLeases(keyId, now);
    const hasLegacy = this._legacyLeases.has(keyId);
    let legacy = [];
    let wasmList = (this._wasmLeases.get(keyId) || []).filter((lease) => lease.expiresAt > now);
    if (hasLegacy) {
      const pruned = this._pruneLeases(keyId, now);
      legacy = pruned.legacy;
      wasmList = pruned.wasmList;
    } else if (wasmList.length) {
      this._wasmLeases.set(keyId, wasmList);
    } else {
      this._wasmLeases.delete(keyId);
    }

    const remainingBudget = legacy.length ? Math.max(0, maxConcurrent - legacy.length) : maxConcurrent;

    let res;
    if (remainingBudget <= MAX_FAST_U31) {
      res = wasm.fastBeginRequest(keyId, remainingBudget, now);
    } else {
      const rpc = wasm.beginRequest({ keyId, maxConcurrent: remainingBudget, now });
      res = { allowed: rpc.allowed, active: rpc.active, leaseId: rpc.lease_id ?? null };
    }

    if (!res.allowed) {
      const combined = legacy.length ? [...legacy, ...wasmList] : wasmList;
      await this.ctx.storage.put(key, { leases: combined });
      return { allowed: false, active: combined.length, leaseId: null };
    }

    wasmList.push({ leaseId: res.leaseId, expiresAt: now + 120_000 });
    this._wasmLeases.set(keyId, wasmList);
    const combined = legacy.length ? [...legacy, ...wasmList] : wasmList;
    await this.ctx.storage.put(key, { leases: combined });
    return { allowed: true, active: combined.length, leaseId: res.leaseId };
  }

  async renewRequest({ keyId, leaseId, ttlMs = 120_000, now = Date.now() }) {
    const key = `active:${keyId}`;
    const wasm = await this._getWasm();
    if (!wasm) {
      const current = await this.ctx.storage.get(key);
      const leases = current?.leases || [];
      const lease = leases.find((entry) => entry.leaseId === leaseId);
      const live = leases.filter((entry) => entry.expiresAt > now).length;
      if (!lease || lease.expiresAt <= now) return { renewed: false, active: live };
      lease.expiresAt = now + ttlMs;
      await this.ctx.storage.put(key, { leases });
      return { renewed: true, active: live, expiresAt: lease.expiresAt };
    }

    await this._hydrateLeases(keyId, now);
    const legacy = this._legacyLeases.get(keyId) || [];
    const wasmList = this._wasmLeases.get(keyId) || [];
    const legacyLive = legacy.filter((entry) => entry.expiresAt > now).length;
    const legacyTarget = legacy.find((entry) => entry.leaseId === leaseId);
    if (legacyTarget) {
      const live = legacyLive + wasmList.filter((entry) => entry.expiresAt > now).length;
      if (legacyTarget.expiresAt <= now) return { renewed: false, active: live };
      legacyTarget.expiresAt = now + ttlMs;
      await this.ctx.storage.put(key, { leases: [...legacy, ...wasmList] });
      return { renewed: true, active: live, expiresAt: legacyTarget.expiresAt };
    }

    const res = wasm.renewRequest({ keyId, leaseId, ttlMs, now });
    const totalLive = res.active + legacyLive;
    if (!res.renewed) {
      return { renewed: false, active: totalLive };
    }
    const wasmTarget = wasmList.find((entry) => entry.leaseId === leaseId);
    if (wasmTarget) wasmTarget.expiresAt = res.expires_at;
    await this.ctx.storage.put(key, { leases: [...legacy, ...wasmList] });
    return { renewed: true, active: totalLive, expiresAt: res.expires_at };
  }

  async endRequest({ keyId, leaseId }) {
    const key = `active:${keyId}`;
    const wasm = await this._getWasm();
    if (!wasm) {
      const current = await this.ctx.storage.get(key);
      const active = (current?.leases || []).filter((lease) => lease.leaseId !== leaseId);
      if (active.length) await this.ctx.storage.put(key, { leases: active });
      else await this.ctx.storage.delete(key);
      return { active: active.length };
    }

    if (!this._hydratedLeases.has(keyId)) {
      const stored = await this.ctx.storage.get(key);
      this._hydratedLeases.add(keyId);
      if (Array.isArray(stored?.leases) && stored.leases.length) {
        this._legacyLeases.set(keyId, stored.leases);
      }
    }

    let legacy = this._legacyLeases.get(keyId);
    if (legacy) {
      const hadLegacy = legacy.some((entry) => entry.leaseId === leaseId);
      if (hadLegacy) {
        legacy = legacy.filter((entry) => entry.leaseId !== leaseId);
        if (legacy.length) this._legacyLeases.set(keyId, legacy);
        else {
          this._legacyLeases.delete(keyId);
          legacy = null;
        }
      } else {
        wasm.fastEndRequest(keyId, leaseId);
      }
    } else {
      wasm.fastEndRequest(keyId, leaseId);
    }

    const wasmList = (this._wasmLeases.get(keyId) || []).filter((entry) => entry.leaseId !== leaseId);
    if (wasmList.length) this._wasmLeases.set(keyId, wasmList);
    else this._wasmLeases.delete(keyId);

    const active = legacy && legacy.length ? [...legacy, ...wasmList] : wasmList;
    if (active.length) await this.ctx.storage.put(key, { leases: active });
    else await this.ctx.storage.delete(key);
    return { active: active.length };
  }

  async _hydrateLockProvider(provider) {
    if (this._hydratedLockProviders.has(provider)) return;
    const storedGen = Number(await this.ctx.storage.get(`lockgen:${provider}`)) || 0;
    this._lockGenBase.set(provider, storedGen);
    this._hydratedLockProviders.add(provider);
  }

  async _hydrateLockKey(lockKey) {
    if (this._hydratedLockKeys.has(lockKey)) return;
    const existing = await this.ctx.storage.get(lockKey);
    this._hydratedLockKeys.add(lockKey);
    if (existing) this._legacyLocks.set(lockKey, existing);
  }

  async setLock({ provider, connectionId, model, untilMs, status, reason, now = Date.now() }) {
    const lockKey = model ? `lock:${provider}:${connectionId}:${model}` : `lock:${provider}:${connectionId}:*`;
    const wasm = await this._getWasm();
    if (!wasm) {
      const key = `lockgen:${provider}`;
      const generation = (Number(await this.ctx.storage.get(key)) || 0) + 1;
      await this.ctx.storage.put(key, generation);
      const payload = { untilMs, status, reason, generation, setAt: now };
      await this.ctx.storage.put(lockKey, payload);
      return { locked: true, untilMs, generation };
    }

    await this._hydrateLockProvider(provider);
    const res = wasm.setLock({
      provider,
      connectionId,
      model: model || undefined,
      untilMs,
      status: status ?? 0,
      reason: reason ?? "",
      now,
    });
    const generation = res.generation + (this._lockGenBase.get(provider) || 0);
    const payload = { untilMs, status, reason, generation, setAt: now };
    this._legacyLocks.delete(lockKey);
    this._lockMirrors.set(lockKey, payload);
    this._hydratedLockKeys.add(lockKey);
    await this.ctx.storage.put(`lockgen:${provider}`, generation);
    await this.ctx.storage.put(lockKey, payload);
    return { locked: true, untilMs, generation };
  }

  async clearLock({ provider, connectionId, model, expectedGeneration }) {
    const lockKey = model ? `lock:${provider}:${connectionId}:${model}` : `lock:${provider}:${connectionId}:*`;
    const wasm = await this._getWasm();
    if (!wasm) {
      const existing = await this.ctx.storage.get(lockKey);
      if (expectedGeneration != null && existing && Number(existing.generation || 0) > Number(expectedGeneration)) {
        return { cleared: false, reason: "newer_lock", generation: existing.generation };
      }
      await this.ctx.storage.delete(lockKey);
      return { cleared: true };
    }

    await this._hydrateLockProvider(provider);
    await this._hydrateLockKey(lockKey);

    const existing = this._lockMirrors.get(lockKey) || this._legacyLocks.get(lockKey);
    if (expectedGeneration != null && existing && Number(existing.generation || 0) > Number(expectedGeneration)) {
      return { cleared: false, reason: "newer_lock", generation: existing.generation };
    }

    const baseGen = this._lockGenBase.get(provider) || 0;
    const wasmExpected = expectedGeneration != null ? Math.max(0, Number(expectedGeneration) - baseGen) : -1;
    if (typeof wasm.fastClearLock === "function") {
      wasm.fastClearLock(provider, connectionId, model || "", wasmExpected);
    } else {
      wasm.clearLock({ provider, connectionId, model: model || undefined, expectedGeneration: wasmExpected >= 0 ? wasmExpected : null });
    }

    this._legacyLocks.delete(lockKey);
    this._lockMirrors.delete(lockKey);
    await this.ctx.storage.delete(lockKey);
    return { cleared: true };
  }

  async getActiveLocks({ provider, connectionIds, model, now = Date.now() }) {
    const wasm = await this._getWasm();
    if (!wasm) {
      const generation = Number(await this.ctx.storage.get(`lockgen:${provider}`)) || 0;
      const locks = await Promise.all(connectionIds.map(async (connectionId) => {
        const keys = [model, "*"].filter(Boolean);
        const found = await Promise.all(keys.map((lockModel) => this.ctx.storage.get(`lock:${provider}:${connectionId}:${lockModel}`)));
        const active = found.find((lock) => lock && lock.untilMs > now);
        return active ? { connectionId, untilMs: active.untilMs, status: active.status, reason: active.reason, generation: active.generation, setAt: active.setAt } : null;
      }));
      return { locks: locks.filter(Boolean), generation };
    }

    await this._hydrateLockProvider(provider);
    const modelsToCheck = [model, "*"].filter(Boolean);
    const coldLookups = [];
    for (const connectionId of connectionIds) {
      for (const lockModel of modelsToCheck) {
        const lockKey = `lock:${provider}:${connectionId}:${lockModel}`;
        if (!this._hydratedLockKeys.has(lockKey)) {
          coldLookups.push(this._hydrateLockKey(lockKey));
        }
      }
    }
    if (coldLookups.length) await Promise.all(coldLookups);

    const baseGen = this._lockGenBase.get(provider) || 0;
    const wasmRes = wasm.getActiveLocks({
      provider,
      connectionIds,
      model: model || undefined,
      now,
    });

    const wasmLocksByCid = new Map();
    for (const lock of wasmRes.locks || []) {
      const cid = lock.connection_id;
      const mirrorKey = model ? `lock:${provider}:${cid}:${model}` : `lock:${provider}:${cid}:*`;
      const mirror = this._lockMirrors.get(mirrorKey) || this._lockMirrors.get(`lock:${provider}:${cid}:*`);
      wasmLocksByCid.set(cid, {
        connectionId: cid,
        untilMs: lock.until_ms,
        status: mirror ? mirror.status : lock.status,
        reason: mirror ? mirror.reason : lock.reason,
        generation: lock.generation + baseGen,
        setAt: lock.set_at,
      });
    }

    const locks = [];
    let maxGen = Math.max(baseGen, (wasmRes.generation || 0) + baseGen);
    for (const cid of connectionIds) {
      const wLock = wasmLocksByCid.get(cid);
      if (wLock) {
        locks.push(wLock);
        continue;
      }
      for (const lockModel of modelsToCheck) {
        const legacy = this._legacyLocks.get(`lock:${provider}:${cid}:${lockModel}`);
        if (legacy && legacy.untilMs > now) {
          maxGen = Math.max(maxGen, Number(legacy.generation || 0));
          locks.push({
            connectionId: cid,
            untilMs: legacy.untilMs,
            status: legacy.status,
            reason: legacy.reason,
            generation: legacy.generation,
            setAt: legacy.setAt,
          });
          break;
        }
      }
    }

    return { locks, generation: maxGen };
  }

  async _hydrateScore(wasm, provider, connectionId, model) {
    const key = `score:${provider}:${connectionId}:${model}`;
    if (this._hydratedScores.has(key)) return;
    const previous = await this.ctx.storage.get(key);
    this._hydratedScores.add(key);
    if (previous && Number(previous.samples) > 0) {
      this._presentScores.add(key);
      wasm.recordOutcomes({
        items: [
          {
            provider: previous.provider || provider,
            connection_id: previous.connectionId || connectionId,
            model: previous.model || model,
            samples: Number(previous.samples) || 0,
            successes: Number(previous.successes) || 0,
            failures: Number(previous.failures) || 0,
            tail_failures: Number(previous.consecutiveFailures) || 0,
            latency_sum_ms: Number(previous.ewmaLatencyMs) || 0,
            latency_count: 1,
            cost_usd: Number(previous.totalCostUsd) || 0,
            last_status: Number(previous.lastStatus) || 0,
          },
        ],
        now: Number(previous.updatedAt) || 0,
      });
    }
  }

  async recordOutcome({ provider, connectionId, model, success, latencyMs, status, costUsd = 0, now = Date.now() }) {
    const key = `score:${provider}:${connectionId}:${model}`;
    const wasm = await this._getWasm();
    if (!wasm) {
      const previous = await this.ctx.storage.get(key);
      const samples = Number(previous?.samples) || 0;
      const successes = Number(previous?.successes) || 0;
      const failures = Number(previous?.failures) || 0;
      const priorLatency = Number(previous?.ewmaLatencyMs) || 0;
      const latency = Number.isFinite(latencyMs) ? Math.max(Number(latencyMs), 0) : priorLatency;
      const ewmaLatencyMs = samples === 0 ? latency : priorLatency * 0.7 + latency * 0.3;
      const entry = {
        provider,
        connectionId,
        model,
        samples: samples + 1,
        successes: successes + (success ? 1 : 0),
        failures: failures + (success ? 0 : 1),
        consecutiveFailures: success ? 0 : (Number(previous?.consecutiveFailures) || 0) + 1,
        ewmaLatencyMs,
        totalCostUsd: (Number(previous?.totalCostUsd) || 0) + Math.max(Number(costUsd) || 0, 0),
        lastStatus: Number(status) || 0,
        updatedAt: now,
      };
      await this.ctx.storage.put(key, entry);
      return entry;
    }

    await this._hydrateScore(wasm, provider, connectionId, model);
    const raw = wasm.recordOutcome({
      provider,
      connectionId,
      model,
      success: Boolean(success),
      latencyMs: Number.isFinite(latencyMs) ? Math.max(Number(latencyMs), 0) : undefined,
      status: Number(status) || 0,
      costUsd: Math.max(Number(costUsd) || 0, 0),
      now,
    });
    const entry = mapScoreEntry(raw);
    this._presentScores.add(key);
    await this.ctx.storage.put(key, entry);
    return entry;
  }

  /**
   * Batched form of recordOutcome. Each item is the pre-aggregated delta of
   * many outcomes for one (provider, connection, model):
   *   { provider, connectionId, model, samples, successes, failures,
   *     tailFailures, latencySumMs, latencyCount, costUsd, lastStatus }
   * `tailFailures` is the run of failures at the end of the batch. One storage
   * read + write per distinct key replaces one per request.
   */
  async recordOutcomes({ items = [], now = Date.now() } = {}) {
    const wasm = await this._getWasm();
    if (!wasm) {
      let applied = 0;
      for (const item of items) {
        if (!item?.provider || !item?.connectionId || !(Number(item.samples) > 0)) continue;
        const key = `score:${item.provider}:${item.connectionId}:${item.model}`;
        const previous = await this.ctx.storage.get(key);
        const priorSamples = Number(previous?.samples) || 0;
        const priorLatency = Number(previous?.ewmaLatencyMs) || 0;
        const latencyCount = Number(item.latencyCount) || 0;
        const meanLatency = latencyCount > 0 ? Math.max(Number(item.latencySumMs) || 0, 0) / latencyCount : priorLatency;
        const decay = Math.pow(0.7, latencyCount);
        const ewmaLatencyMs = priorSamples === 0 ? meanLatency : priorLatency * decay + meanLatency * (1 - decay);
        const successes = Number(item.successes) || 0;
        const failures = Number(item.failures) || 0;
        await this.ctx.storage.put(key, {
          provider: item.provider,
          connectionId: item.connectionId,
          model: item.model,
          samples: priorSamples + Number(item.samples),
          successes: (Number(previous?.successes) || 0) + successes,
          failures: (Number(previous?.failures) || 0) + failures,
          consecutiveFailures: successes === 0 ? (Number(previous?.consecutiveFailures) || 0) + failures : Number(item.tailFailures) || 0,
          ewmaLatencyMs,
          totalCostUsd: (Number(previous?.totalCostUsd) || 0) + Math.max(Number(item.costUsd) || 0, 0),
          lastStatus: Number(item.lastStatus) || 0,
          updatedAt: now,
        });
        applied += 1;
      }
      return { recorded: applied };
    }

    const valid = items.filter((item) => item?.provider && item?.connectionId && Number(item.samples) > 0);
    if (!valid.length) return { recorded: 0 };

    const uniqueTargets = new Map();
    const wasmItems = [];
    for (const item of valid) {
      const key = `score:${item.provider}:${item.connectionId}:${item.model}`;
      if (!uniqueTargets.has(key)) {
        uniqueTargets.set(key, { provider: item.provider, connectionId: item.connectionId, model: item.model });
        await this._hydrateScore(wasm, item.provider, item.connectionId, item.model);
      }
      wasmItems.push({
        provider: item.provider,
        connection_id: item.connectionId,
        model: item.model,
        samples: Number(item.samples) || 0,
        successes: Number(item.successes) || 0,
        failures: Number(item.failures) || 0,
        tail_failures: Number(item.tailFailures) || 0,
        latency_sum_ms: Math.max(Number(item.latencySumMs) || 0, 0),
        latency_count: Number(item.latencyCount) || 0,
        cost_usd: Math.max(Number(item.costUsd) || 0, 0),
        last_status: Number(item.lastStatus) || 0,
      });
    }

    const res = wasm.recordOutcomes({ items: wasmItems, now });
    const groups = new Map();
    for (const target of uniqueTargets.values()) {
      const gk = `${target.provider}\0${target.model}`;
      if (!groups.has(gk)) groups.set(gk, { provider: target.provider, model: target.model, connectionIds: [] });
      groups.get(gk).connectionIds.push(target.connectionId);
    }

    for (const group of groups.values()) {
      const scores = wasm.getScores({
        provider: group.provider,
        connectionIds: group.connectionIds,
        model: group.model,
        now,
      });
      for (const raw of scores || []) {
        const entry = mapScoreEntry(raw);
        const key = `score:${entry.provider}:${entry.connectionId}:${entry.model}`;
        this._presentScores.add(key);
        await this.ctx.storage.put(key, entry);
      }
    }

    return { recorded: res?.recorded ?? valid.length };
  }

  async getScores({ provider, connectionIds, model, now = Date.now() }) {
    const wasm = await this._getWasm();
    if (!wasm) {
      const entries = await Promise.all(connectionIds.map(async (connectionId) => {
        const entry = await this.ctx.storage.get(`score:${provider}:${connectionId}:${model}`);
        if (!entry) return null;
        if (now - Number(entry.updatedAt || 0) > SCORE_TTL_MS) {
          await this.ctx.storage.delete(`score:${provider}:${connectionId}:${model}`);
          return null;
        }
        return entry;
      }));
      return entries.filter(Boolean);
    }

    for (const connectionId of connectionIds) {
      await this._hydrateScore(wasm, provider, connectionId, model);
    }

    const rawEntries = wasm.getScores({ provider, connectionIds, model, now }) || [];
    const activeEntries = rawEntries.map(mapScoreEntry);
    const activeIds = new Set(activeEntries.map((entry) => entry.connectionId));

    for (const connectionId of connectionIds) {
      const key = `score:${provider}:${connectionId}:${model}`;
      if (!activeIds.has(connectionId) && this._presentScores.has(key)) {
        this._presentScores.delete(key);
        await this.ctx.storage.delete(key);
      }
    }

    return activeEntries;
  }

  async _hydrateBudget(wasm, keyId, now) {
    if (this._hydratedBudgets.has(keyId)) return;
    const current = await this.ctx.storage.get(`budget:${keyId}`);
    this._hydratedBudgets.add(keyId);
    const active = (current?.reservations || []).filter((entry) => entry.expiresAt > now);
    if (active.length) {
      for (const entry of active) {
        wasm.reserveBudget({
          keyId,
          requestId: entry.requestId,
          amount: Number(entry.amount) || 0,
          expiresAt: entry.expiresAt,
          availableBudget: null,
          now,
        });
      }
      this._budgetMirrors.set(keyId, active);
    }
  }

  async reserveBudget({ keyId, requestId, amount, expiresAt, availableBudget, now = Date.now() }) {
    const key = `budget:${keyId}`;
    const wasm = await this._getWasm();
    if (!wasm) {
      const current = await this.ctx.storage.get(key);
      const reservations = (current?.reservations || []).filter((entry) => entry.expiresAt > now);
      const existing = reservations.find((entry) => entry.requestId === requestId);
      if (existing) {
        await this.ctx.storage.put(key, { reservations });
        return { reserved: true, amount: existing.amount, requestId };
      }
      const reservedAmount = reservations.reduce((total, entry) => total + Number(entry.amount || 0), 0);
      if (availableBudget != null && availableBudget !== "" && Number.isFinite(Number(availableBudget)) && reservedAmount + Number(amount) > Number(availableBudget)) {
        return { reserved: false, reservedAmount };
      }
      reservations.push({ requestId, amount, expiresAt });
      await this.ctx.storage.put(key, { reservations });
      return { reserved: true, amount, requestId };
    }

    await this._hydrateBudget(wasm, keyId, now);
    const mirror = (this._budgetMirrors.get(keyId) || []).filter((entry) => entry.expiresAt > now);
    const hasAvail = availableBudget != null && availableBudget !== "" && Number.isFinite(Number(availableBudget));
    const res = wasm.reserveBudget({
      keyId,
      requestId,
      amount: Number(amount),
      expiresAt,
      availableBudget: hasAvail ? Number(availableBudget) : null,
      now,
    });

    if (!res.reserved) {
      if (mirror.length) this._budgetMirrors.set(keyId, mirror);
      else this._budgetMirrors.delete(keyId);
      return { reserved: false, reservedAmount: res.reserved_amount ?? 0 };
    }

    if (!mirror.some((entry) => entry.requestId === requestId)) {
      mirror.push({ requestId, amount, expiresAt });
    }
    this._budgetMirrors.set(keyId, mirror);
    await this.ctx.storage.put(key, { reservations: mirror });
    return { reserved: true, amount: res.amount, requestId };
  }

  async releaseBudget({ keyId, requestId, now = Date.now() }) {
    const key = `budget:${keyId}`;
    const wasm = await this._getWasm();
    if (!wasm) {
      const current = await this.ctx.storage.get(key);
      const reservations = (current?.reservations || []).filter((entry) => entry.requestId !== requestId && entry.expiresAt > now);
      if (reservations.length) await this.ctx.storage.put(key, { reservations });
      else await this.ctx.storage.delete(key);
      return { released: true, reservations };
    }

    await this._hydrateBudget(wasm, keyId, now);
    wasm.releaseBudget({ keyId, requestId });
    const reservations = (this._budgetMirrors.get(keyId) || []).filter((entry) => entry.requestId !== requestId && entry.expiresAt > now);
    if (reservations.length) {
      this._budgetMirrors.set(keyId, reservations);
      await this.ctx.storage.put(key, { reservations });
    } else {
      this._budgetMirrors.delete(keyId);
      await this.ctx.storage.delete(key);
    }
    return { released: true, reservations };
  }

  async getBudgetReservations({ keyId, now = Date.now() }) {
    const wasm = await this._getWasm();
    if (!wasm) {
      const current = await this.ctx.storage.get(`budget:${keyId}`);
      return (current?.reservations || []).filter((entry) => entry.expiresAt > now);
    }

    await this._hydrateBudget(wasm, keyId, now);
    const reservations = (this._budgetMirrors.get(keyId) || []).filter((entry) => entry.expiresAt > now);
    if (reservations.length) this._budgetMirrors.set(keyId, reservations);
    else this._budgetMirrors.delete(keyId);
    return reservations;
  }

  /**
   * Configures fast-path routing tables inside the WASM edge engine (with JS fallback).
   */
  async configureRouting({ apiKeys = [], aliases = {}, combos = [], connections = [] } = {}) {
    this._routingConfig = { apiKeys, aliases, combos, connections };
    const wasm = await this._getWasm();
    if (wasm) {
      wasm.configureRouting(this._routingConfig);
    }
    return { configured: true, wasm: Boolean(wasm) };
  }

  /**
   * Resolves API key auth, rate limit, concurrency lease, alias, combo strategy,
   * lock filtering, and upstream connection in a single WASM edge call (with JS fallback).
   */
  async resolveRoute({ model, apiKey, path = "/v1/chat/completions", now = Date.now() } = {}) {
    const wasm = await this._getWasm();
    if (wasm) {
      const raw = wasm.resolveRoute({ model, apiKey, path, now });
      return {
        allowed: Boolean(raw.allowed),
        status: raw.status,
        errorMessage: raw.error_message ?? null,
        resolvedModel: raw.resolved_model,
        selectedProvider: raw.selected_provider,
        selectedConnectionId: raw.selected_connection_id,
        baseUrl: raw.base_url ?? null,
        leaseId: raw.lease_id ?? null,
        rateLimitRemaining: raw.rate_limit_remaining,
        rateLimitResetAt: raw.rate_limit_reset_at,
        isHedged: Boolean(raw.is_hedged),
        backupModel: raw.backup_model ?? null,
        backupProvider: raw.backup_provider ?? null,
        backupConnectionId: raw.backup_connection_id ?? null,
      };
    }

    return await this._resolveRouteJs({ model, apiKey, now });
  }

  async _resolveRouteJs({ model, apiKey, now }) {
    const cfg = this._routingConfig || { apiKeys: [], aliases: {}, combos: [], connections: [] };
    let leaseId = null;
    let rateLimitRemaining = Number.MAX_SAFE_INTEGER;
    let rateLimitResetAt = 0;

    let keyRecord = null;
    if (apiKey != null) {
      const found = (cfg.apiKeys || []).find((k) => k.key === apiKey);
      if (!found || !found.is_active) {
        return {
          allowed: false,
          status: 401,
          errorMessage: found ? "API key is inactive" : "Invalid API key",
          resolvedModel: model,
          selectedProvider: "",
          selectedConnectionId: "",
          baseUrl: null,
          leaseId: null,
          rateLimitRemaining: 0,
          rateLimitResetAt: 0,
          isHedged: false,
          backupModel: null,
          backupProvider: null,
          backupConnectionId: null,
        };
      }
      keyRecord = found;
    }

    if (keyRecord) {
      const allowedModels = keyRecord.allowed_models || [];
      if (allowedModels.length && !allowedModels.includes("*") && !allowedModels.includes(model)) {
        return {
          allowed: false,
          status: 403,
          errorMessage: `Model '${model}' not permitted for this key`,
          resolvedModel: model,
          selectedProvider: "",
          selectedConnectionId: "",
          baseUrl: null,
          leaseId: null,
          rateLimitRemaining: 0,
          rateLimitResetAt: 0,
          isHedged: false,
          backupModel: null,
          backupProvider: null,
          backupConnectionId: null,
        };
      }
      const rl = await this.checkRateLimit({
        keyId: keyRecord.id,
        limit: keyRecord.rate_limit ?? 10_000,
        windowMs: keyRecord.rate_limit_window_ms ?? 60_000,
        now,
      });
      rateLimitRemaining = rl.remaining;
      rateLimitResetAt = rl.resetAt;
      if (!rl.allowed) {
        return {
          allowed: false,
          status: 429,
          errorMessage: "Rate limit exceeded",
          resolvedModel: model,
          selectedProvider: "",
          selectedConnectionId: "",
          baseUrl: null,
          leaseId: null,
          rateLimitRemaining: rl.remaining,
          rateLimitResetAt: rl.resetAt,
          isHedged: false,
          backupModel: null,
          backupProvider: null,
          backupConnectionId: null,
        };
      }
      const lease = await this.beginRequest({
        keyId: keyRecord.id,
        maxConcurrent: keyRecord.max_concurrent ?? 100,
        now,
      });
      if (!lease.allowed) {
        return {
          allowed: false,
          status: 429,
          errorMessage: "Concurrency limit exceeded",
          resolvedModel: model,
          selectedProvider: "",
          selectedConnectionId: "",
          baseUrl: null,
          leaseId: null,
          rateLimitRemaining: rl.remaining,
          rateLimitResetAt: rl.resetAt,
          isHedged: false,
          backupModel: null,
          backupProvider: null,
          backupConnectionId: null,
        };
      }
      leaseId = lease.leaseId;
    }

    const targetModel = cfg.aliases?.[model] || model;
    const combo = (cfg.combos || []).find((c) => c.name === targetModel);
    let resolvedModel = targetModel;
    let isHedged = false;
    let backupModel = null;

    if (combo && Array.isArray(combo.models) && combo.models.length) {
      const effective = [];
      for (const cand of combo.models) {
        const conns = (cfg.connections || []).filter((c) => c.model === cand && c.is_active !== false);
        if (!conns.length) {
          effective.push(cand);
          continue;
        }
        let unlocked = false;
        for (const conn of conns) {
          const active = await this.getActiveLocks({
            provider: conn.provider,
            connectionIds: [conn.connection_id],
            model: cand,
            now,
          });
          if (!active.locks.length) {
            unlocked = true;
            break;
          }
        }
        if (unlocked) effective.push(cand);
      }
      const pool = effective.length ? effective : combo.models;
      const strategy = combo.strategy || "fallback";
      if (strategy === "round-robin") {
        const idx = this._rrCounters.get(combo.name) || 0;
        resolvedModel = pool[idx % pool.length];
        this._rrCounters.set(combo.name, (idx + 1) % pool.length);
      } else if (strategy === "sticky") {
        const stickyKey = `${combo.name}:${keyRecord?.id || "anon"}`;
        const prev = this._stickyPicks.get(stickyKey);
        resolvedModel = prev && pool.includes(prev) ? prev : pool[0];
        this._stickyPicks.set(stickyKey, resolvedModel);
      } else if (strategy === "hedged-racing" || strategy === "fusion") {
        isHedged = true;
        resolvedModel = pool[0];
        if (pool.length > 1) backupModel = pool[1];
      } else {
        resolvedModel = pool[0];
      }
    }

    const modelConns = (cfg.connections || []).filter((c) => c.model === resolvedModel && c.is_active !== false);
    let chosen = modelConns[0] || null;
    for (const c of modelConns) {
      const active = await this.getActiveLocks({ provider: c.provider, connectionIds: [c.connection_id], model: resolvedModel, now });
      if (!active.locks.length) {
        chosen = c;
        break;
      }
    }

    const backupConn = backupModel
      ? (cfg.connections || []).find((c) => c.model === backupModel && c.is_active !== false) || null
      : null;

    return {
      allowed: true,
      status: 200,
      errorMessage: null,
      resolvedModel,
      selectedProvider: chosen?.provider || "direct",
      selectedConnectionId: chosen?.connection_id || "conn-0",
      baseUrl: chosen?.base_url ?? null,
      leaseId,
      rateLimitRemaining,
      rateLimitResetAt,
      isHedged,
      backupModel,
      backupProvider: backupConn?.provider ?? null,
      backupConnectionId: backupConn?.connection_id ?? null,
    };
  }

  async bumpVersion({ domain }) {
    const key = `version:${domain}`;
    const current = Number(await this.ctx.storage.get(key)) || 0;
    const next = current + 1;
    await this.ctx.storage.put(key, next);
    return { version: next };
  }

  /** Versions for several domains in one call (one RPC instead of one per domain). */
  async getVersions({ domains = [] } = {}) {
    const versions = {};
    for (const domain of domains) versions[domain] = Number(await this.ctx.storage.get(`version:${domain}`)) || 0;
    return { versions };
  }

  async getVersion({ domain }) {
    return { version: Number(await this.ctx.storage.get(`version:${domain}`)) || 0 };
  }

  async recordMetrics({ outcome }) {
    const now = Number(outcome?.now) || Date.now();
    if (!outcome?.provider) return { recorded: false };
    // Tenant is part of the row identity: one upstream connection can be healthy
    // for one tenant and failing for another, and per-connection verdicts cannot
    // answer "who is affected right now".
    const tenant = typeof outcome.tenant === "string" && outcome.tenant.trim() ? outcome.tenant.trim() : "anonymous";
    const key = `metrics:outcome:${tenant}:${outcome.provider}:${outcome.connectionId}:${outcome.model}`;
    const previous = await this.ctx.storage.get(key);
    const current = previous || {
      tenant,
      provider: outcome.provider,
      connectionId: outcome.connectionId,
      model: outcome.model,
      successes: 0,
      failures: 0,
      consecutiveFailures: 0,
      lastStatus: 0,
      lastLatencyMs: 0,
      updatedAt: now,
    };
    current.successes += outcome.success ? 1 : 0;
    current.failures += outcome.success ? 0 : 1;
    current.consecutiveFailures = outcome.success ? 0 : current.consecutiveFailures + 1;
    current.lastStatus = Number(outcome.status) || 0;
    if (Number.isFinite(outcome.latencyMs)) current.lastLatencyMs = Math.max(Number(outcome.latencyMs), 0);
    current.updatedAt = now;
    await this.ctx.storage.put(key, current);

    const totalsKey = "metrics:totals";
    const totals = await this.ctx.storage.get(totalsKey);
    const next = totals || { startedAt: now, requests: 0, successes: 0, failures: 0, latencyCount: 0, latencySumMs: 0, latencyMaxMs: 0 };
    next.requests += 1;
    if (outcome.success) next.successes += 1;
    else next.failures += 1;
    if (Number.isFinite(outcome.latencyMs)) {
      const latency = Math.max(Number(outcome.latencyMs), 0);
      next.latencyCount += 1;
      next.latencySumMs += latency;
      next.latencyMaxMs = Math.max(next.latencyMaxMs, latency);
    }
    await this.ctx.storage.put(totalsKey, next);

    // Per-tenant rollup, so a tenant-scoped read does not have to sum every
    // provider row (and so the counters can be labelled per tenant).
    const tenantTotalsKey = `metrics:tenant:${tenant}`;
    const tenantTotals = (await this.ctx.storage.get(tenantTotalsKey)) || { tenant, requests: 0, successes: 0, failures: 0 };
    tenantTotals.requests += 1;
    if (outcome.success) tenantTotals.successes += 1;
    else tenantTotals.failures += 1;
    await this.ctx.storage.put(tenantTotalsKey, tenantTotals);

    return { recorded: true };
  }

  /**
   * Batched form of recordMetrics. Items are per-(tenant, provider,
   * connection, model) deltas:
   *   { tenant, provider, connectionId, model, successes, failures,
   *     tailFailures, lastStatus, lastLatencyMs, latencyCount, latencySumMs,
   *     latencyMaxMs }
   * The cluster totals and per-tenant rollups are read and written once per
   * batch instead of once per request.
   */
  async recordMetricsBatch({ items = [], now = Date.now() } = {}) {
    const valid = items.filter((item) => item?.provider && (Number(item.successes) || 0) + (Number(item.failures) || 0) > 0);
    if (!valid.length) return { recorded: 0 };
    const totalsKey = "metrics:totals";
    const totals = (await this.ctx.storage.get(totalsKey)) || { startedAt: now, requests: 0, successes: 0, failures: 0, latencyCount: 0, latencySumMs: 0, latencyMaxMs: 0 };
    const tenantTotals = new Map();
    for (const item of valid) {
      const tenant = typeof item.tenant === "string" && item.tenant.trim() ? item.tenant.trim() : "anonymous";
      const successes = Number(item.successes) || 0;
      const failures = Number(item.failures) || 0;
      const key = `metrics:outcome:${tenant}:${item.provider}:${item.connectionId}:${item.model}`;
      const previous = await this.ctx.storage.get(key);
      const current = previous || { tenant, provider: item.provider, connectionId: item.connectionId, model: item.model, successes: 0, failures: 0, consecutiveFailures: 0, lastStatus: 0, lastLatencyMs: 0, updatedAt: now };
      current.successes += successes;
      current.failures += failures;
      current.consecutiveFailures = successes === 0 ? current.consecutiveFailures + failures : Number(item.tailFailures) || 0;
      current.lastStatus = Number(item.lastStatus) || 0;
      if (Number.isFinite(item.lastLatencyMs)) current.lastLatencyMs = Math.max(Number(item.lastLatencyMs), 0);
      current.updatedAt = now;
      await this.ctx.storage.put(key, current);

      totals.requests += successes + failures;
      totals.successes += successes;
      totals.failures += failures;
      if (Number(item.latencyCount) > 0) {
        totals.latencyCount += Number(item.latencyCount);
        totals.latencySumMs += Math.max(Number(item.latencySumMs) || 0, 0);
        totals.latencyMaxMs = Math.max(totals.latencyMaxMs, Number(item.latencyMaxMs) || 0);
      }
      const rollup = tenantTotals.get(tenant) || { tenant, requests: 0, successes: 0, failures: 0 };
      rollup.requests += successes + failures;
      rollup.successes += successes;
      rollup.failures += failures;
      tenantTotals.set(tenant, rollup);
    }
    await this.ctx.storage.put(totalsKey, totals);
    for (const [tenant, delta] of tenantTotals) {
      const key = `metrics:tenant:${tenant}`;
      const stored = (await this.ctx.storage.get(key)) || { tenant, requests: 0, successes: 0, failures: 0 };
      stored.requests += delta.requests;
      stored.successes += delta.successes;
      stored.failures += delta.failures;
      await this.ctx.storage.put(key, stored);
    }
    return { recorded: valid.length };
  }

  async getMetrics({ tenant } = {}) {
    const all = await this.ctx.storage.list({ prefix: "metrics:" });
    const providers = [];
    const tenantTotals = new Map();
    let totals = null;
    for (const [key, value] of all.entries()) {
      if (key === "metrics:totals") totals = value;
      else if (key.startsWith("metrics:tenant:")) tenantTotals.set(value?.tenant || key.slice("metrics:tenant:".length), value);
      else if (key.startsWith("metrics:outcome:")) {
        // A tenant-scoped read must not return rows belonging to other tenants.
        if (tenant && value?.tenant !== tenant) continue;
        providers.push(value);
      }
    }
    const latency = {
      count: totals?.latencyCount || 0,
      sumMs: totals?.latencySumMs || 0,
      maxMs: totals?.latencyMaxMs || 0,
      averageMs: totals?.latencyCount ? totals.latencySumMs / totals.latencyCount : 0,
    };
    const alerts = providers
      .filter((entry) => entry.consecutiveFailures >= 3 || (entry.successes + entry.failures >= 5 && entry.failures / (entry.successes + entry.failures) >= 0.5))
      .map((entry) => ({
        fingerprint: `${entry.tenant || "anonymous"}:${entry.provider}:${entry.connectionId}:${entry.model}`,
        severity: entry.consecutiveFailures >= 5 ? "critical" : "warning",
        type: "provider_degraded",
        tenant: entry.tenant || "anonymous",
        provider: entry.provider,
        model: entry.model,
        connectionId: entry.connectionId,
        consecutiveFailures: entry.consecutiveFailures,
        observedAt: entry.updatedAt,
      }));
    // Counters are labelled per tenant: the unfiltered view aggregates the
    // cluster, a filtered one reports only that tenant's numbers.
    const counters = tenant
      ? (() => {
          const t = tenantTotals.get(tenant);
          return [
            { name: "provider_requests_total", labels: { tenant }, value: t?.requests || 0 },
            { name: "provider_success_total", labels: { tenant }, value: t?.successes || 0 },
            { name: "provider_failure_total", labels: { tenant }, value: t?.failures || 0 },
          ];
        })()
      : [
          { name: "provider_requests_total", labels: {}, value: totals?.requests || 0 },
          { name: "provider_success_total", labels: {}, value: totals?.successes || 0 },
          { name: "provider_failure_total", labels: {}, value: totals?.failures || 0 },
        ];

    return {
      startedAt: totals?.startedAt || Date.now(),
      ...(tenant ? { tenant } : {}),
      counters,
      providers,
      // Latency is tracked cluster-wide only, so a tenant-scoped read must not
      // present it as that tenant's; null says "not tracked per tenant" instead
      // of implying zero.
      latency: tenant ? { count: null, sumMs: null, maxMs: null, averageMs: null } : latency,
      alerts,
    };
  }

  async snapshot() {
    const all = await this.ctx.storage.list();
    return Array.from(all.entries()).map(([key, value]) => ({ key, value }));
  }
}
