// Persistence profile for metered stores (Cloudflare D1 + the coordinator
// Durable Object, both SQLite-backed and billed per row read / written).
//
// "extreme" is the default on the Cloudflare runtime and keeps every
// per-request hot path off D1 and (as far as correctness allows) off the
// Durable Object:
//   - operational state (model locks, round-robin rotation, proxy quarantine)
//     lives in isolate memory; locks are still published to the coordinator so
//     other isolates see them;
//   - per-request outcome/metrics reports to the coordinator are coalesced into
//     one batched call per flush interval instead of 2-3 storage writes each;
//   - coordinator reads on the request path (active locks, adaptive scores,
//     control-version polls) are cached for a short TTL;
//   - the large `kv` table hydrates per scope, not as a whole;
//   - a rejected API key cannot force a D1 read per attempt.
// Credentials, token refresh, settings, API keys and the explicit budget ledger
// stay durable and write-through.
//
// Override with NINEROUTER_D1_PROFILE=standard (Worker var) to restore the
// previous behaviour, or set it to "extreme" on a non-Cloudflare process to
// exercise the same code paths locally (used by the tests).
const PROFILES = new Set(["extreme", "standard"]);

export function writeProfile() {
  const requested = String(process.env.NINEROUTER_D1_PROFILE || "").trim().toLowerCase();
  if (PROFILES.has(requested)) return requested;
  return process.env.NINEROUTER_RUNTIME === "cloudflare" ? "extreme" : "standard";
}

export function isExtremeProfile() {
  return writeProfile() === "extreme";
}

function intFromEnv(name, fallback) {
  const raw = Number(process.env[name]);
  return Number.isFinite(raw) && raw >= 0 ? raw : fallback;
}

/** Tunables for the extreme profile. All overridable by env for ops without a redeploy of code. */
export function extremeTunables() {
  return {
    // How long a cached coordinator lock/score answer may be served.
    coordinatorReadTtlMs: intFromEnv("NINEROUTER_COORDINATOR_READ_TTL_MS", 5_000),
    // Longest a coalesced outcome/metric batch waits before it is sent.
    coordinatorFlushMs: intFromEnv("NINEROUTER_COORDINATOR_FLUSH_MS", 30_000),
    // Send early when this many outcome events are pending.
    coordinatorFlushMaxEvents: intFromEnv("NINEROUTER_COORDINATOR_FLUSH_MAX", 500),
    // Control-version (key revocation / kill switch) poll period.
    controlPollMs: intFromEnv("NINEROUTER_CONTROL_POLL_MS", 10_000),
    // Minimum gap between forced API-key refreshes triggered by unknown keys.
    unknownKeyRefreshMs: intFromEnv("NINEROUTER_UNKNOWN_KEY_REFRESH_MS", 30_000),
  };
}
