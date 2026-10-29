// Cloudflare-build stub for "undici" (see next.config.mjs, resolve.alias).
// Workers have a native fetch and no connection-level dispatchers, so undici's ~1.2 MB of
// source is dead weight. Only the callers that survive on Workers need a shape:
//  * Agent: open-sse/translator/concerns/image.js pins DNS with an Agent and passes it to the
//    global fetch, which ignores `dispatcher` on Workers. A harmless placeholder keeps that
//    path behaving exactly as it did with real undici there.
//  * ProxyAgent / proxied fetch: proxy pools cannot work on Workers (proxyFetch.js skips them
//    when NINEROUTER_RUNTIME=cloudflare). Fail loudly instead of silently testing a direct
//    connection and reporting a proxy as healthy.
class Agent {
  constructor(options) { this.options = options; }
  close() { return Promise.resolve(); }
  destroy() { return Promise.resolve(); }
}
class ProxyAgent {
  constructor() {
    throw new Error("Proxy dispatchers are not available in the Cloudflare Worker build");
  }
}
function fetch(input, init) {
  if (init && init.dispatcher) {
    return Promise.reject(new Error("Proxy dispatchers are not available in the Cloudflare Worker build"));
  }
  return globalThis.fetch(input, init);
}
module.exports = { Agent, ProxyAgent, fetch };
