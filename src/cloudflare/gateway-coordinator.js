import { DurableObject } from "cloudflare:workers";

export class GatewayCoordinator extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.env = env;
    this.ctx = ctx;
  }

  async checkRateLimit({ keyId, limit, windowMs, now = Date.now() }) {
    const key = `rate:${keyId}`;
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

  async beginRequest({ keyId, maxConcurrent, now = Date.now() }) {
    const key = `active:${keyId}`;
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

  async endRequest({ keyId, leaseId }) {
    const key = `active:${keyId}`;
    const current = await this.ctx.storage.get(key);
    const active = (current?.leases || []).filter((lease) => lease.leaseId !== leaseId);
    if (active.length) await this.ctx.storage.put(key, { leases: active });
    else await this.ctx.storage.delete(key);
    return { active: active.length };
  }

  async setLock({ provider, connectionId, model, untilMs, status, reason }) {
    const keys = [model ? `lock:${provider}:${connectionId}:${model}` : `lock:${provider}:${connectionId}:*`];
    await Promise.all(keys.map((key) => this.ctx.storage.put(key, { untilMs, status, reason })));
    return { locked: true, untilMs };
  }

  async clearLock({ provider, connectionId, model }) {
    const key = model ? `lock:${provider}:${connectionId}:${model}` : `lock:${provider}:${connectionId}:*`;
    await this.ctx.storage.delete(key);
    return { cleared: true };
  }

  async getActiveLocks({ provider, connectionIds, model, now = Date.now() }) {
    const entries = await Promise.all(connectionIds.map(async (connectionId) => {
      const keys = [model, "*"].filter(Boolean);
      const locks = await Promise.all(keys.map((lockModel) => this.ctx.storage.get(`lock:${provider}:${connectionId}:${lockModel}`)));
      const active = locks.find((lock) => lock && lock.untilMs > now);
      return active ? { connectionId, untilMs: active.untilMs, status: active.status, reason: active.reason } : null;
    }));
    return entries.filter(Boolean);
  }

  async recordOutcome({ provider, connectionId, model, success, latencyMs, status, costUsd = 0, now = Date.now() }) {
    const key = `score:${provider}:${connectionId}:${model}`;
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

  async getScores({ provider, connectionIds, model, now = Date.now() }) {
    const entries = await Promise.all(connectionIds.map(async (connectionId) => {
      const entry = await this.ctx.storage.get(`score:${provider}:${connectionId}:${model}`);
      if (!entry) return null;
      if (now - Number(entry.updatedAt || 0) > 86_400_000) {
        await this.ctx.storage.delete(`score:${provider}:${connectionId}:${model}`);
        return null;
      }
      return entry;
    }));
    return entries.filter(Boolean);
  }

  async reserveBudget({ keyId, requestId, amount, expiresAt, availableBudget, now = Date.now() }) {
    const key = `budget:${keyId}`;
    const current = await this.ctx.storage.get(key);
    const reservations = (current?.reservations || []).filter((entry) => entry.expiresAt > now);
    const existing = reservations.find((entry) => entry.requestId === requestId);
    if (existing) {
      await this.ctx.storage.put(key, { reservations });
      return { reserved: true, amount: existing.amount, requestId };
    }
    const reservedAmount = reservations.reduce((total, entry) => total + Number(entry.amount || 0), 0);
    if (Number.isFinite(Number(availableBudget)) && reservedAmount + Number(amount) > Number(availableBudget)) {
      return { reserved: false, reservedAmount };
    }
    reservations.push({ requestId, amount, expiresAt });
    await this.ctx.storage.put(key, { reservations });
    return { reserved: true, amount, requestId };
  }

  async releaseBudget({ keyId, requestId, now = Date.now() }) {
    const key = `budget:${keyId}`;
    const current = await this.ctx.storage.get(key);
    const reservations = (current?.reservations || []).filter((entry) => entry.requestId !== requestId && entry.expiresAt > now);
    if (reservations.length) await this.ctx.storage.put(key, { reservations });
    else await this.ctx.storage.delete(key);
    return { released: true, reservations };
  }

  async getBudgetReservations({ keyId, now = Date.now() }) {
    const current = await this.ctx.storage.get(`budget:${keyId}`);
    return (current?.reservations || []).filter((entry) => entry.expiresAt > now);
  }

  async snapshot() {
    const all = await this.ctx.storage.list();
    return Array.from(all.entries()).map(([key, value]) => ({ key, value }));
  }
}
