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

  // Read-only twin of checkRateLimit for dashboards: same window math, no
  // increment, so polling "remaining" never burns budget.
  async peekRateLimit({ keyId, limit, windowMs, now = Date.now() }) {
    const key = `rate:${keyId}`;
    const current = await this.ctx.storage.get(key);
    const active = current && now - current.windowStart < windowMs ? current : { count: 0, windowStart: now };
    const resetAt = active.windowStart + windowMs;
    return {
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

  async renewRequest({ keyId, leaseId, ttlMs = 120_000, now = Date.now() }) {
    const key = `active:${keyId}`;
    const current = await this.ctx.storage.get(key);
    const leases = current?.leases || [];
    const lease = leases.find((entry) => entry.leaseId === leaseId);
    const live = leases.filter((entry) => entry.expiresAt > now).length;
    // An already-expired lease is presumed abandoned: its slot may since have
    // been handed to another request, so resurrecting it would double-count.
    if (!lease || lease.expiresAt <= now) return { renewed: false, active: live };
    lease.expiresAt = now + ttlMs;
    await this.ctx.storage.put(key, { leases });
    return { renewed: true, active: live, expiresAt: lease.expiresAt };
  }

  async endRequest({ keyId, leaseId }) {
    const key = `active:${keyId}`;
    const current = await this.ctx.storage.get(key);
    const active = (current?.leases || []).filter((lease) => lease.leaseId !== leaseId);
    if (active.length) await this.ctx.storage.put(key, { leases: active });
    else await this.ctx.storage.delete(key);
    return { active: active.length };
  }

  async setLock({ provider, connectionId, model, untilMs, status, reason, now = Date.now() }) {
    const key = `lockgen:${provider}`;
    const generation = (Number(await this.ctx.storage.get(key)) || 0) + 1;
    await this.ctx.storage.put(key, generation);
    const lockKey = model ? `lock:${provider}:${connectionId}:${model}` : `lock:${provider}:${connectionId}:*`;
    const payload = { untilMs, status, reason, generation, setAt: now };
    await this.ctx.storage.put(lockKey, payload);
    return { locked: true, untilMs, generation };
  }

  async clearLock({ provider, connectionId, model, expectedGeneration }) {
    const lockKey = model ? `lock:${provider}:${connectionId}:${model}` : `lock:${provider}:${connectionId}:*`;
    const existing = await this.ctx.storage.get(lockKey);
    // A success may only clear a lock it actually observed. A failure that
    // landed after this request started is newer evidence and must survive.
    if (expectedGeneration != null && existing && Number(existing.generation || 0) > Number(expectedGeneration)) {
      return { cleared: false, reason: "newer_lock", generation: existing.generation };
    }
    await this.ctx.storage.delete(lockKey);
    return { cleared: true };
  }

  async getActiveLocks({ provider, connectionIds, model, now = Date.now() }) {
    const generation = Number(await this.ctx.storage.get(`lockgen:${provider}`)) || 0;
    const locks = await Promise.all(connectionIds.map(async (connectionId) => {
      const keys = [model, "*"].filter(Boolean);
      const found = await Promise.all(keys.map((lockModel) => this.ctx.storage.get(`lock:${provider}:${connectionId}:${lockModel}`)));
      const active = found.find((lock) => lock && lock.untilMs > now);
      return active ? { connectionId, untilMs: active.untilMs, status: active.status, reason: active.reason, generation: active.generation, setAt: active.setAt } : null;
    }));
    return { locks: locks.filter(Boolean), generation };
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

  /**
   * Batched form of recordOutcome. Each item is the pre-aggregated delta of
   * many outcomes for one (provider, connection, model):
   *   { provider, connectionId, model, samples, successes, failures,
   *     tailFailures, latencySumMs, latencyCount, costUsd, lastStatus }
   * `tailFailures` is the run of failures at the end of the batch. One storage
   * read + write per distinct key replaces one per request.
   */
  async recordOutcomes({ items = [], now = Date.now() } = {}) {
    let applied = 0;
    for (const item of items) {
      if (!item?.provider || !item?.connectionId || !(Number(item.samples) > 0)) continue;
      const key = `score:${item.provider}:${item.connectionId}:${item.model}`;
      const previous = await this.ctx.storage.get(key);
      const priorSamples = Number(previous?.samples) || 0;
      const priorLatency = Number(previous?.ewmaLatencyMs) || 0;
      const latencyCount = Number(item.latencyCount) || 0;
      const meanLatency = latencyCount > 0 ? Math.max(Number(item.latencySumMs) || 0, 0) / latencyCount : priorLatency;
      // n events at the batch mean latency, folded into the EWMA (weight 0.3 each).
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
