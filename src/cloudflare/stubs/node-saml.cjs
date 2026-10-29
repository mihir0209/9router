// Cloudflare-build stub for "@node-saml/node-saml" (see next.config.mjs, resolve.alias).
// Not usable on Workers (SAML routes are not public on this deployment and need Node crypto/XML tooling); this keeps it out of the Worker bundle.
// Throws only when something actually touches the module.
const message = "@node-saml/node-saml is not available in the Cloudflare Worker build: SAML routes are not public on this deployment and need Node crypto/XML tooling";
module.exports = new Proxy({}, {
  get(_target, prop) {
    if (prop === "__esModule" || typeof prop === "symbol" || prop === "then") return undefined;
    throw new Error(message);
  },
});
