// Cloudflare-build stub for "@aws-sdk/credential-providers" (see next.config.mjs, resolve.alias).
// Not usable on Workers (profile/SSO credential chains read ~/.aws, use static AWS keys on Workers); this keeps it out of the Worker bundle.
// Throws only when something actually touches the module.
const message = "@aws-sdk/credential-providers is not available in the Cloudflare Worker build: profile/SSO credential chains read ~/.aws, use static AWS keys on Workers";
module.exports = new Proxy({}, {
  get(_target, prop) {
    if (prop === "__esModule" || typeof prop === "symbol" || prop === "then") return undefined;
    throw new Error(message);
  },
});
