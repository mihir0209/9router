import { NextResponse } from "next/server";
import { getProvider } from "@/lib/proxy/providers/registry.js";
import { getProviderConfigs, refreshProviderPool } from "@/lib/proxy/refreshPool.js";

/**
 * POST /api/proxy-pools/providers/[id]/refresh
 * Trigger an immediate refresh for a single provider.
 */
export async function POST(_req, { params }) {
  try {
    const { id } = params;
    const provider = await getProvider(id);
    if (!provider) return NextResponse.json({ error: "Unknown provider" }, { status: 404 });

    const configs = await getProviderConfigs();
    const config = configs[id] || provider.defaults;

    const result = await refreshProviderPool(id, config);
    return NextResponse.json(result);
  } catch (err) {
    return NextResponse.json({ error: err.message }, { status: 500 });
  }
}
