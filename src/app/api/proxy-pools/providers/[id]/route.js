import { NextResponse } from "next/server";
import { getProvider, getAllProviders } from "@/lib/proxy/providers/registry.js";
import { getProviderConfigs, updateProviderConfig } from "@/lib/proxy/refreshPool.js";

/**
 * GET /api/proxy-pools/providers/[id]
 * Get a single provider's config.
 */
export async function GET(_req, { params }) {
  try {
    const { id } = params;
    const provider = await getProvider(id);
    if (!provider) return NextResponse.json({ error: "Unknown provider" }, { status: 404 });

    const configs = await getProviderConfigs();
    const config = { ...provider.defaults, ...(configs[id] || {}) };

    return NextResponse.json({
      id: provider.id,
      name: provider.name,
      description: provider.description,
      configFields: provider.configFields,
      config,
      enabled: config.enabled || false,
    });
  } catch (err) {
    return NextResponse.json({ error: err.message }, { status: 500 });
  }
}

/**
 * PUT /api/proxy-pools/providers/[id]
 * Update a single provider's config.
 * Body: { enabled, ...config fields }
 */
export async function PUT(req, { params }) {
  try {
    const { id } = params;
    const provider = await getProvider(id);
    if (!provider) return NextResponse.json({ error: "Unknown provider" }, { status: 404 });

    const body = await req.json();
    const normalized = provider.normalizeConfig(body);
    await updateProviderConfig(id, normalized);

    return NextResponse.json({ ok: true, config: normalized });
  } catch (err) {
    return NextResponse.json({ error: err.message }, { status: 500 });
  }
}

/**
 * DELETE /api/proxy-pools/providers/[id]
 * Disable a provider (does not delete config).
 */
export async function DELETE(_req, { params }) {
  try {
    const { id } = params;
    const configs = await getProviderConfigs();
    if (configs[id]) {
      configs[id].enabled = false;
      await updateProviderConfig(id, configs[id]);
    }
    return NextResponse.json({ ok: true });
  } catch (err) {
    return NextResponse.json({ error: err.message }, { status: 500 });
  }
}
