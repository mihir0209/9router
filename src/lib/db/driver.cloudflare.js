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

export async function getAdapter() {
  if (state.instance) return state.instance;
  if (!state.initPromise) state.initPromise = initAdapter().then((a) => { state.instance = a; return a; });
  return state.initPromise;
}

export function getAdapterSync() {
  if (!state.instance) throw new Error("[DB] adapter not initialized - await getAdapter() first");
  return state.instance;
}
