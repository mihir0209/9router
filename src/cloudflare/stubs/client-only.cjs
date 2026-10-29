// Cloudflare-build stub for browser-only UI libraries (recharts, @xyflow/react).
// They are imported only from components loaded with next/dynamic({ ssr: false }), which are
// never executed on the server, yet webpack still compiles those lazy chunks into the Worker
// bundle (several hundred KB). Server-side they resolve here; the browser bundle is unaffected.
const message = "This UI library is client-only and is not bundled into the Cloudflare Worker";
module.exports = new Proxy({}, {
  get(_target, prop) {
    if (prop === "__esModule" || typeof prop === "symbol" || prop === "then") return undefined;
    throw new Error(message);
  },
});
