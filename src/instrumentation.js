export async function register() {
  if (process.env.NEXT_RUNTIME === "nodejs") {
    // Publish the dashboard JWT secret into the environment before the first
    // request. The proxy runs in the Edge runtime, which has no filesystem, so
    // it can only verify the session token against process.env — without this
    // the Edge side uses a different key and every dashboard request is
    // bounced back to /login.
    const { ensureJwtSecretEnv } = await import("@/lib/auth/jwtSecret");
    ensureJwtSecretEnv();

    const { initConsoleLogCapture } = await import("@/lib/consoleLogBuffer");
    initConsoleLogCapture();

    // Server-only: lets capabilities.js read the synced catalog without pulling
    // node:fs into the dashboard's browser bundle.
    const { installCatalogSource } = await import("open-sse/providers/catalogOverride.js");
    await installCatalogSource();

    const { startModelCatalogSync } = await import("@/lib/modelCatalog/sync.js");
    startModelCatalogSync();
  }
}
