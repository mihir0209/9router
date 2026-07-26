import { NextResponse } from "next/server";
import { getAllProviders } from "@/lib/proxy/providers/registry.js";
import { getProviderConfigs, updateProviderConfig, refreshProviderPool, refreshAllEnabledPools } from "@/lib/proxy/refreshPool.js";

/**
 * GET /api/proxy-pools/providers
 * List all registered providers with their configs.
 */
export async function GET() {
  try {
    const providers = getAllProviders();
    const configs = await getProviderConfigs();

    const result = providers.map((p) => ({
      id: p.id,
      name: p.name,
      description: p.description,
      configFields: p.configFields,
      config: { ...p.defaults, ...(configs[p.id] || {}) },
      enabled: configs[p.id]?.enabled || false,
    }));

    return NextResponse.json(result);
  } catch (err) {
    return NextResponse.json({ error: err.message }, { status: 500 });
  }
}

/**
 * POST /api/proxy-pools/providers
 * Update one or more provider configs.
 * Body: { providers: { [id]: { enabled, ...config } } }
 * Or: { providerId, config } for single update.
 */
export async function POST(req) {
  try {
    const body = await req.json();

    // Refresh all enabled
    if (body.action === "refresh-all") {
      const results = await refreshAllEnabledPools();
      return NextResponse.json({ results });
    }

    // Single provider refresh
    if (body.action === "refresh" && body.providerId) {
      const result = await refreshProviderPool(body.providerId, body.config || {});
      return NextResponse.json(result);
    }

    // Bulk update configs
    if (body.providers && typeof body.providers === "object") {
      for (const [id, config] of Object.entries(body.providers)) {
        await updateProviderConfig(id, config);
      }
      return NextResponse.json({ ok: true });
    }

    // Single update
    if (body.providerId && body.config) {
      await updateProviderConfig(body.providerId, body.config);
      return NextResponse.json({ ok: true });
    }

    return NextResponse.json({ error: "Invalid request body" }, { status: 400 });
  } catch (err) {
    return NextResponse.json({ error: err.message }, { status: 500 });
  }
}
