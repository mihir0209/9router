import { NextResponse } from "next/server";
import {
  getProviderConnections,
  getSettings,
  normalizeModelsListSource,
  saveProviderModelCache,
  clearProviderModelCache,
  getAllProviderModelCaches,
} from "@/lib/localDb";
import { resolveProviderModelList, hasLiveModelSource } from "@/lib/modelCatalog/providerModelResolver.js";

export const dynamic = "force-dynamic";

/**
 * GET /api/models/refresh — current cache state, no upstream calls.
 *
 * ?provider=<id> returns that provider's model ids as well, so the provider
 * page can render exactly what /v1/models will serve. Ids are scoped to one
 * provider on purpose: a summary for every provider stays a few hundred bytes,
 * whereas returning all of them would be megabytes on a page that loads on
 * every provider view.
 */
export async function GET(request) {
  try {
    const only = new URL(request.url).searchParams.get("provider");
    const [caches, settings, connections] = await Promise.all([
      getAllProviderModelCaches(),
      getSettings(),
      getProviderConnections(),
    ]);
    const active = new Set(
      connections.filter((c) => c.isActive !== false).map((c) => c.provider)
    );
    const entries = (only ? [[only, caches[only]]] : Object.entries(caches))
      .filter(([provider, entry]) => entry && (only || entry))
      .map(([provider, entry]) => ({
        provider,
        count: entry.models.length,
        fetchedAt: entry.fetchedAt,
        providerActive: active.has(provider),
        canRefresh: hasLiveModelSource(provider, connections.find((c) => c.provider === provider)),
        ...(only ? { ids: entry.models.map((m) => m.id) } : {}),
      }));
    return NextResponse.json({
      modelsListSource: normalizeModelsListSource(settings.modelsListSource),
      providers: entries,
    });
  } catch (error) {
    console.error("[API] Failed to read model cache state:", error);
    return NextResponse.json({ error: "Failed to read model cache state" }, { status: 500 });
  }
}

/**
 * POST /api/models/refresh — re-resolve model lists and persist them.
 *
 * Body: { provider?: string }
 *   provider given → refresh just that one
 *   omitted        → refresh every active connection that has a live source
 *
 * This is the only place that writes the cache, so the list served in "cached"
 * mode can never drift from what a live pass would produce.
 */
export async function POST(request) {
  try {
    let provider = null;
    try {
      const body = await request.json();
      provider = typeof body?.provider === "string" && body.provider.trim() !== ""
        ? body.provider.trim()
        : null;
    } catch {
      // An empty body is valid: it means "refresh everything".
    }

    const connections = await getProviderConnections();
    const active = connections.filter((c) => c.isActive !== false);

    // One connection per provider, mirroring how the model list is built.
    const byProvider = new Map();
    for (const conn of active) {
      if (!byProvider.has(conn.provider)) byProvider.set(conn.provider, conn);
    }

    const targets = provider ? [provider] : Array.from(byProvider.keys());
    if (provider && !byProvider.has(provider)) {
      return NextResponse.json(
        { error: `No active connection for provider "${provider}"` },
        { status: 404 }
      );
    }

    const results = [];
    for (const target of targets) {
      const conn = byProvider.get(target);
      if (!conn) continue;
      if (!hasLiveModelSource(target, conn)) {
        // No discovery path for this provider (a fixed catalog, or no apiKey on
        // a compatible node). Its cache is not meaningful, so drop any stale
        // entry rather than serving a list that can never be refreshed.
        if (provider) await clearProviderModelCache(target);
        results.push({ provider: target, refreshed: false, reason: "no-live-source", count: 0 });
        continue;
      }
      try {
        const resolved = await resolveProviderModelList(conn, target, { allowLive: true });
        // A live pass that found nothing is a failure, not an empty list:
        // saveProviderModelCache refuses to overwrite a good cache with that.
        const saved = await saveProviderModelCache(target, resolved.models, {
          fetchedAt: new Date().toISOString(),
        });
        results.push({
          provider: target,
          refreshed: saved.stored,
          count: saved.count,
          source: resolved.source,
          reason: saved.reason,
          truncated: saved.truncated === true,
          totalDiscovered: saved.totalDiscovered,
          error: resolved.error,
        });
      } catch (error) {
        console.error(`[API] Model refresh failed for ${target}:`, error);
        results.push({ provider: target, refreshed: false, reason: "error", error: error?.message });
      }
    }

    return NextResponse.json({ ok: true, results });
  } catch (error) {
    console.error("[API] Failed to refresh models:", error);
    return NextResponse.json({ error: "Failed to refresh models" }, { status: 500 });
  }
}
