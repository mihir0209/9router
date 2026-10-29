import { mapWithConcurrency } from "./mapWithConcurrency.js";

// A Worker may hold only 6 simultaneous outbound connections per request; further fetches queue
// behind them and a deadlock-prone pile-up stalls the whole request. Dashboard fan-outs (proxy
// health checks, model tests, ...) therefore stay at or below 5 on Cloudflare. Other runtimes keep
// whatever concurrency the caller configured. Override: NINEROUTER_OUTBOUND_CONCURRENCY.
export const CLOUDFLARE_OUTBOUND_CONCURRENCY = 5;

export function capOutboundConcurrency(requested) {
  const wanted = Math.max(1, Math.floor(Number(requested)) || 1);
  if (process.env.NINEROUTER_RUNTIME !== "cloudflare") return wanted;
  const override = Number(process.env.NINEROUTER_OUTBOUND_CONCURRENCY);
  const cap = Number.isFinite(override) && override >= 1 ? Math.floor(override) : CLOUDFLARE_OUTBOUND_CONCURRENCY;
  return Math.min(wanted, cap);
}

const NO_TIMEOUT_MS = 2 ** 31 - 1;

/**
 * Promise.all(items.map(fn)) with at most `concurrency` calls in flight. Results keep input
 * order and the first rejection rejects the whole call, exactly like Promise.all (no per-task
 * timeout, no fallback value, unlike plain mapWithConcurrency).
 */
export function mapLimited(items, fn, concurrency) {
  return mapWithConcurrency(items, fn, {
    concurrency,
    timeoutMs: NO_TIMEOUT_MS,
    onError: (error) => { throw error; },
  });
}
