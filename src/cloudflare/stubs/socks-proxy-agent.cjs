// Cloudflare-build stub for "socks-proxy-agent" (see next.config.mjs, resolve.alias).
// Not usable on Workers (SOCKS proxies need raw TCP sockets, which Workers do not give to proxy agents); this keeps it out of the Worker bundle.
// Throws only when something actually touches the module.
const message = "socks-proxy-agent is not available in the Cloudflare Worker build: SOCKS proxies need raw TCP sockets, which Workers do not give to proxy agents";
module.exports = new Proxy({}, {
  get(_target, prop) {
    if (prop === "__esModule" || typeof prop === "symbol" || prop === "then") return undefined;
    throw new Error(message);
  },
});
