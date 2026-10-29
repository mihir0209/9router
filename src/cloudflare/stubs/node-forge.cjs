// Cloudflare-build stub for "node-forge" (see next.config.mjs, resolve.alias).
// Not usable on Workers (the local MITM certificate tooling has no place on Workers); this keeps it out of the Worker bundle.
// Throws only when something actually touches the module.
const message = "node-forge is not available in the Cloudflare Worker build: the local MITM certificate tooling has no place on Workers";
module.exports = new Proxy({}, {
  get(_target, prop) {
    if (prop === "__esModule" || typeof prop === "symbol" || prop === "then") return undefined;
    throw new Error(message);
  },
});
