/**
 * Proxy performance metrics — tracks latency, success rates, and health scores.
 */

const metrics = new Map();

const DEFAULT_MIN_REQUESTS_FOR_SCORING = 5;

function getMetrics(proxyId) {
  if (!metrics.has(proxyId)) {
    metrics.set(proxyId, {
      totalRequests: 0,
      successfulRequests: 0,
      failedRequests: 0,
      totalLatency: 0,
      minLatency: Infinity,
      maxLatency: 0,
      lastRequestTime: null,
      lastSuccessTime: null,
      lastFailureTime: null,
      consecutiveFailures: 0,
    });
  }
  return metrics.get(proxyId);
}

export function recordRequest(proxyId, success, latencyMs) {
  const m = getMetrics(proxyId);
  m.totalRequests++;
  m.lastRequestTime = Date.now();

  if (success) {
    m.successfulRequests++;
    m.totalLatency += latencyMs;
    m.minLatency = Math.min(m.minLatency, latencyMs);
    m.maxLatency = Math.max(m.maxLatency, latencyMs);
    m.lastSuccessTime = Date.now();
    m.consecutiveFailures = 0;
  } else {
    m.failedRequests++;
    m.lastFailureTime = Date.now();
    m.consecutiveFailures++;
  }
}

export function getHealthScore(proxyId) {
  const m = getMetrics(proxyId);

  if (m.totalRequests < DEFAULT_MIN_REQUESTS_FOR_SCORING) {
    return 50;
  }

  const successRate = m.successfulRequests / m.totalRequests;
  const avgLatency = m.totalLatency / m.successfulRequests || 0;

  let score = successRate * 70;

  if (avgLatency < 1000) score += 20;
  else if (avgLatency < 3000) score += 10;
  else if (avgLatency < 5000) score += 5;

  if (m.consecutiveFailures > 0) {
    score -= m.consecutiveFailures * 10;
  }

  return Math.max(0, Math.min(100, Math.round(score)));
}

export function getMetricsSnapshot(proxyId) {
  const m = getMetrics(proxyId);
  return { ...m, healthScore: getHealthScore(proxyId) };
}

export function resetMetrics(proxyId) {
  metrics.delete(proxyId);
}

export function getAllMetrics() {
  const result = {};
  for (const [id, m] of metrics) {
    result[id] = { ...m, healthScore: getHealthScore(id) };
  }
  return result;
}
