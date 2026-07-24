/**
 * Optional built-in retry plugins. Import this module (side effect) to register.
 * Not auto-imported so tests stay deterministic; wire from app bootstrap if needed.
 */
import { registerRetryPlugin } from "../index.js";

// Example: free-tier style providers often return 200-ish errors with plain text quotas
registerRetryPlugin({
  id: "openai-compatible-",
  matchMode: "prefix",
  errorRules: [
    { text: "insufficient tokens", backoff: true },
    { text: "free daily limit", cooldownMs: 60 * 60 * 1000 },
  ],
});

registerRetryPlugin({
  id: "anthropic-compatible-",
  matchMode: "prefix",
  errorRules: [
    { text: "overloaded_error", backoff: true },
  ],
});
