// Counting stand-in for the GATEWAY_COORDINATOR Durable Object namespace.
// It runs the real GatewayCoordinator class (src/cloudflare/gateway-coordinator.js)
// over a Map-backed storage and counts:
//   rpcs          calls into the Durable Object (each is a billed DO request)
//   rowsRead      storage.get / list entries returned
//   rowsWritten   storage.put / delete operations
// The caller must vi.mock("cloudflare:workers") with a DurableObject base class.
export async function createCountingDO() {
  const { GatewayCoordinator } = await import("../../src/cloudflare/gateway-coordinator.js");
  const counters = { rpcs: 0, rowsRead: 0, rowsWritten: 0, byMethod: {} };
  const instances = new Map();

  function storageFor(map) {
    return {
      async get(key) { counters.rowsRead += 1; return map.has(key) ? structuredClone(map.get(key)) : undefined; },
      async put(key, value) { counters.rowsWritten += 1; map.set(key, structuredClone(value)); },
      async delete(key) { counters.rowsWritten += 1; return map.delete(key); },
      async list({ prefix = "" } = {}) {
        const out = new Map();
        for (const [k, v] of map) if (k.startsWith(prefix)) out.set(k, structuredClone(v));
        counters.rowsRead += out.size;
        return out;
      },
    };
  }

  function instance(scope) {
    if (!instances.has(scope)) {
      const map = new Map();
      instances.set(scope, new GatewayCoordinator({ storage: storageFor(map) }, {}));
    }
    return instances.get(scope);
  }

  const namespace = {
    getByName(scope) {
      const target = instance(scope || "global");
      return new Proxy({}, {
        get(_t, method) {
          if (typeof target[method] !== "function") return undefined;
          return async (payload) => {
            counters.rpcs += 1;
            counters.byMethod[method] = (counters.byMethod[method] || 0) + 1;
            return target[method](payload);
          };
        },
      });
    },
  };

  return {
    namespace,
    snapshot: () => JSON.parse(JSON.stringify(counters)),
    reset() { counters.rpcs = 0; counters.rowsRead = 0; counters.rowsWritten = 0; counters.byMethod = {}; },
  };
}
