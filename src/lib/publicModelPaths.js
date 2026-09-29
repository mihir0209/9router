// Which model-list paths are readable without an API key.
//
// The model catalogue is public: a client needs it to know what it can call,
// often before it holds a key, and it contains no user data. Every other LLM
// route still requires a key.
//
// This list is imported by src/proxy.cloudflare.js and by
// scripts/build-cloudflare.mjs. That matters because the Workers build injects
// its own copy of the auth policy into worker.js, since that guard has to run
// before the Next middleware. When both sides hardcoded the list, the source
// edit shipped, the build contained it, and production kept enforcing the old
// policy because the injected copy answered first. Importing one list means the
// two agree by construction.
export const PUBLIC_MODEL_PATHS = [
  "/v1/models",
  "/api/v1/models",
  "/v1beta/models",
  "/api/v1beta/models",
];

// Clients are inconsistent about a trailing slash, and /v1/models and
// /v1/models/ are the same resource. Answering 401 for one and 200 for the
// other is a trap for whoever hits it first.
export function isPublicModelPath(pathname) {
  const normalized = pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname;
  return PUBLIC_MODEL_PATHS.includes(normalized);
}
