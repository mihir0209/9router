import pkg from "../../../../package.json" with { type: "json" };

const NPM_PACKAGE_NAME = "9router";
const VERSION_CACHE_TTL_MS = 3600000; // cache npm latest lookup for 1h

// Survive hot reload; one cache per process
const versionCache = (global.__npmVersionCache ??= { value: null, fetchedAt: 0 });

// fetch() works on Node and Workers; node:https is unreliable under
// OpenNext's nodejs_compat + global_fetch_strictly_public and was 500ing.
async function fetchLatestVersion() {
  try {
    const res = await fetch(`https://registry.npmjs.org/${NPM_PACKAGE_NAME}/latest`, {
      headers: { accept: "application/json" },
      signal: AbortSignal.timeout(4000),
      cache: "no-store",
    });
    if (!res.ok) return null;
    const data = await res.json();
    return typeof data?.version === "string" && data.version ? data.version : null;
  } catch {
    return null;
  }
}

function compareVersions(a, b) {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < 3; i++) {
    if (pa[i] > pb[i]) return 1;
    if (pa[i] < pb[i]) return -1;
  }
  return 0;
}

async function getLatestVersionCached() {
  if (versionCache.value && Date.now() - versionCache.fetchedAt < VERSION_CACHE_TTL_MS) {
    return versionCache.value;
  }
  const latest = await fetchLatestVersion();
  if (latest) {
    versionCache.value = latest;
    versionCache.fetchedAt = Date.now();
  }
  return latest;
}

export async function GET() {
  try {
    const latestVersion = await getLatestVersionCached();
    const currentVersion = pkg.version;
    const hasUpdate = latestVersion ? compareVersions(latestVersion, currentVersion) > 0 : false;
    return Response.json({ currentVersion, latestVersion, hasUpdate });
  } catch (error) {
    // Never 500 the version check — dashboard polls this on load.
    return Response.json({
      currentVersion: pkg?.version || null,
      latestVersion: versionCache.value || null,
      hasUpdate: false,
      error: String(error?.message || error),
    });
  }
}
