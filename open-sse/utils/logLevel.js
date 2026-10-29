// Whether INFO-level console lines should be printed. Mirrors the default of
// src/sse/utils/logger.js: INFO everywhere except the Cloudflare runtime, where
// every console line is metered and sampled, so the default there is WARN.
// An explicit LOG_LEVEL always wins.
const LEVEL_RANK = { DEBUG: 0, INFO: 1, WARN: 2, ERROR: 3 };

export function isInfoLogEnabled() {
  const explicit = LEVEL_RANK[process.env.LOG_LEVEL?.toUpperCase?.()];
  if (explicit !== undefined) return explicit <= LEVEL_RANK.INFO;
  return process.env.NINEROUTER_RUNTIME !== "cloudflare";
}
