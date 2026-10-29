import { createFlightGroup } from "../../../open-sse/utils/flight.js";
// Cloudflare Workers cannot bundle native/local SQLite adapters.
// This keeps Worker builds on an ephemeral JS adapter until D1 support is added.
if (!global._dbAdapter) global._dbAdapter = { instance: null, initPromise: null, logged: false };
const state = global._dbAdapter;

async function initAdapter() {
  const { createCloudflareMemoryAdapter } = await import("./adapters/cloudflareMemoryAdapter.js");
  const adapter = await createCloudflareMemoryAdapter();
  if (!state.logged) {
    console.log(`[DB] Driver: ${adapter.driver} | Cloudflare runtime (ephemeral)`);
    state.logged = true;
  }
  return adapter;
}

// Adapter creation is shared between concurrent callers only through a request-safe
// flight (open-sse/utils/flight.js): on Workers a plain shared promise hangs every later
// request of the isolate when the request that started it is cancelled or CPU-killed.
// A failed or abandoned init is retried by the next caller instead of being cached forever.
const initFlights = createFlightGroup({ followerWaitMs: 5000, staleMs: 30000 });

export async function getAdapter() {
  if (state.instance) return state.instance;
  const { value } = await initFlights.run("adapter", async () => {
    if (state.instance) return state.instance;
    const adapter = await initAdapter();
    if (!state.instance) state.instance = adapter;
    return state.instance;
  });
  return value;
}

export function getAdapterSync() {
  if (!state.instance) throw new Error("[DB] adapter not initialized - await getAdapter() first");
  return state.instance;
}
