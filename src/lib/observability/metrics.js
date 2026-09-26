if (!global._gatewayMetrics) {
  global._gatewayMetrics = {
    startedAt: Date.now(),
    counters: new Map(),
    providerOutcomes: new Map(),
    latency: { count: 0, sumMs: 0, maxMs: 0 },
    alerts: [],
  };
}

const state = global._gatewayMetrics;
const METRICS_SCOPE = "metrics";
// Traffic with no API key behind it still has to be attributable, otherwise a
// shared deployment's unattributed errors would look like they belonged to
// whichever tenant happens to be queried.
const UNSCOPED_TENANT = "anonymous";

const tenantOf = (tenant) => (typeof tenant === "string" && tenant.trim() ? tenant.trim() : UNSCOPED_TENANT);

function bump(name, amount = 1, labels = {}) {
  const key = `${name}|${JSON.stringify(labels, Object.keys(labels).sort())}`;
  const current = state.counters.get(key) || { name, labels, value: 0 };
  current.value += amount;
  state.counters.set(key, current);
}

function alertFor(outcome) {
  const total = outcome.successes + outcome.failures;
  if (outcome.consecutiveFailures < 3 && !(total >= 5 && outcome.failures / total >= 0.5)) return null;
  return {
    fingerprint: `${outcome.tenant || UNSCOPED_TENANT}:${outcome.provider}:${outcome.connectionId}:${outcome.model}`,
    severity: outcome.consecutiveFailures >= 5 ? "critical" : "warning",
    type: "provider_degraded",
    tenant: outcome.tenant || UNSCOPED_TENANT,
    provider: outcome.provider,
    model: outcome.model,
    connectionId: outcome.connectionId,
    consecutiveFailures: outcome.consecutiveFailures,
    observedAt: outcome.updatedAt,
  };
}

function trackAlert(alert, fingerprint) {
  state.alerts = state.alerts.filter((entry) => entry.fingerprint !== fingerprint);
  if (alert) state.alerts.push(alert);
}

/**
 * Observability must never break traffic, so the Durable Object write is
 * best-effort: on Workers the DO is the source of truth (every isolate
 * aggregates into it), locally the in-process state is already authoritative.
 */
async function persistOutcome(outcome) {
  if (process.env.NINEROUTER_RUNTIME !== "cloudflare") return false;
  try {
    const { coordinatorCall } = await import("@/lib/coordinator/client.js");
    await coordinatorCall("recordMetrics", { outcome }, METRICS_SCOPE);
    return true;
  } catch {
    return false;
  }
}

export function recordMetric(name, amount = 1, labels = {}) {
  bump(name, amount, labels);
}

export function recordRequestOutcome({ provider, connectionId, model, success, status, latencyMs, tenant }) {
  const scope = tenantOf(tenant);
  recordMetric("provider_requests_total", 1, { tenant: scope, provider, model });
  recordMetric(success ? "provider_success_total" : "provider_failure_total", 1, { tenant: scope, provider, model, status: String(status || 0) });
  if (Number.isFinite(latencyMs)) {
    state.latency.count += 1;
    state.latency.sumMs += Math.max(Number(latencyMs), 0);
    state.latency.maxMs = Math.max(state.latency.maxMs, Math.max(Number(latencyMs), 0));
  }
  // Tenant is part of the identity: the same upstream connection is legitimately
  // healthy for one tenant and failing for another, and a per-connection verdict
  // cannot answer "who is being hurt right now".
  const key = `${scope}:${provider}:${connectionId}:${model}`;
  const current = state.providerOutcomes.get(key) || { tenant: scope, provider, connectionId, model, successes: 0, failures: 0, consecutiveFailures: 0, lastStatus: 0, lastLatencyMs: 0, updatedAt: 0 };
  current.successes += success ? 1 : 0;
  current.failures += success ? 0 : 1;
  current.consecutiveFailures = success ? 0 : current.consecutiveFailures + 1;
  current.lastStatus = Number(status) || 0;
  current.lastLatencyMs = Number.isFinite(latencyMs) ? Math.max(Number(latencyMs), 0) : current.lastLatencyMs;
  current.updatedAt = Date.now();
  state.providerOutcomes.set(key, current);
  trackAlert(alertFor(current), key);
  void persistOutcome({
    tenant: scope,
    provider,
    connectionId,
    model,
    success: success === true,
    status: Number(status) || 0,
    latencyMs: Number.isFinite(latencyMs) ? Math.max(Number(latencyMs), 0) : null,
    now: current.updatedAt,
  });
}

/**
 * Restrict a snapshot to one tenant. Counters and provider rows carry the
 * tenant as a label/field; alerts are matched on it too. Latency is a gateway
 * aggregate rather than a per-tenant series, so it is dropped when filtering
 * (returning a cluster-wide average under a tenant heading would be a lie).
 */
function filterSnapshotByTenant(snapshot, tenant) {
  const scope = tenantOf(tenant);
  const counters = (snapshot.counters || []).filter((c) => tenantOf(c.labels?.tenant) === scope);
  const providers = (snapshot.providers || []).filter((p) => tenantOf(p.tenant) === scope);
  const alerts = (snapshot.alerts || []).filter((a) => tenantOf(a.tenant) === scope);
  return {
    startedAt: snapshot.startedAt,
    now: snapshot.now,
    tenant: scope,
    counters,
    providers,
    latency: counters.length || providers.length
      ? { count: null, sumMs: 0, maxMs: null, averageMs: null }
      : { count: 0, sumMs: 0, maxMs: 0, averageMs: 0 },
    alerts,
  };
}

export function snapshotMetrics() {
  return {
    startedAt: state.startedAt,
    now: Date.now(),
    counters: [...state.counters.values()],
    providers: [...state.providerOutcomes.values()],
    latency: { ...state.latency, averageMs: state.latency.count ? state.latency.sumMs / state.latency.count : 0 },
    alerts: state.alerts,
  };
}

/**
 * Cluster-wide snapshot. On Workers the Durable Object aggregates every
 * isolate, so the local view alone would under-report; the DO view wins when
 * reachable and the local view is the fallback.
 */
export async function getMetricsSnapshot(tenant = null) {
  const local = snapshotMetrics();
  if (!tenant) {
    if (process.env.NINEROUTER_RUNTIME !== "cloudflare") return local;
    try {
      const { coordinatorCall } = await import("@/lib/coordinator/client.js");
      const durable = await coordinatorCall("getMetrics", {}, METRICS_SCOPE);
      if (!durable) return local;
      return {
        startedAt: durable.startedAt || local.startedAt,
        now: Date.now(),
        counters: durable.counters?.length ? durable.counters : local.counters,
        providers: durable.providers?.length ? durable.providers : local.providers,
        latency: durable.latency?.count ? durable.latency : local.latency,
        alerts: durable.alerts || [],
      };
    } catch {
      return local;
    }
  }

  // Tenant-scoped read: the Durable Object is the only place that sees every
  // isolate's numbers, so a per-tenant view has to come from it. When it is
  // unreachable we say so rather than falling back to the local cluster view,
  // which would report the whole gateway under one tenant's name.
  let durable = null;
  try {
    const { coordinatorCall } = await import("@/lib/coordinator/client.js");
    durable = await coordinatorCall("getMetrics", { tenant }, METRICS_SCOPE);
  } catch {
    durable = null;
  }
  if (durable) return filterSnapshotByTenant({ ...durable, now: Date.now() }, tenant);
  return {
    ...filterSnapshotByTenant(local, tenant),
    degraded: "durable-object-unreachable",
  };
}

export async function getAlerts(tenant = null) {
  return (await getMetricsSnapshot(tenant)).alerts;
}

export function resetMetrics() {
  state.startedAt = Date.now();
  state.counters.clear();
  state.providerOutcomes.clear();
  state.latency = { count: 0, sumMs: 0, maxMs: 0 };
  state.alerts = [];
}

export function prometheusMetrics(snapshot = snapshotMetrics()) {
  const escape = (value) => String(value ?? "").replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n");
  const lines = [];
  for (const counter of snapshot.counters) {
    const labels = Object.entries(counter.labels || {}).map(([key, value]) => `${key}="${escape(value)}"`).join(",");
    lines.push(`${counter.name}${labels ? `{${labels}}` : ""} ${counter.value}`);
  }
  for (const provider of snapshot.providers) {
    const labels = `tenant="${escape(provider.tenant || UNSCOPED_TENANT)}",provider="${escape(provider.provider)}",model="${escape(provider.model)}",connection="${escape(provider.connectionId)}"`;
    lines.push(`provider_successes_total{${labels}} ${provider.successes}`);
    lines.push(`provider_failures_total{${labels}} ${provider.failures}`);
    lines.push(`provider_consecutive_failures{${labels}} ${provider.consecutiveFailures}`);
    lines.push(`provider_last_latency_ms{${labels}} ${provider.lastLatencyMs}`);
  }
  lines.push(`gateway_latency_average_ms ${snapshot.latency.averageMs}`);
  lines.push(`gateway_latency_max_ms ${snapshot.latency.maxMs}`);
  lines.push(`gateway_alerts ${snapshot.alerts.length}`);
  return `${lines.join("\n")}\n`;
}
