import { NextResponse } from "next/server";
import { getProviderConnectionById, updateProviderConnection } from "@/lib/localDb";
import { isAnthropicCompatibleProvider, isOpenAICompatibleProvider } from "@/shared/constants/providers";
import { pingModelByKind } from "@/app/api/models/test/ping";
import { getDefaultModel, getProviderModels, PROVIDER_ID_TO_ALIAS } from "open-sse/config/providerModels.js";

async function testCloudflareConnection(request, id) {
  const connection = await getProviderConnectionById(id);
  if (!connection) return { valid: false, error: "Connection not found" };

  const startedAt = Date.now();
  let result;

  if (isOpenAICompatibleProvider(connection.provider) || isAnthropicCompatibleProvider(connection.provider)) {
    const baseUrl = connection.providerSpecificData?.baseUrl?.replace(/\/$/, "");
    if (!baseUrl) {
      result = { valid: false, error: "Missing base URL" };
    } else {
      const headers = {};
      if (connection.apiKey) {
        headers.Authorization = `Bearer ${connection.apiKey}`;
        headers["x-api-key"] = connection.apiKey;
      }
      const response = await fetch(`${baseUrl}/models`, { headers, signal: AbortSignal.timeout(15000) });
      const text = await response.text().catch(() => "");
      result = {
        valid: response.ok,
        error: response.ok ? null : `HTTP ${response.status}${text ? `: ${text.slice(0, 240)}` : ""}`,
      };
    }
  } else {
    const alias = PROVIDER_ID_TO_ALIAS[connection.provider] || connection.provider;
    const enabledModels = connection.providerSpecificData?.enabledModels;
    const model = connection.defaultModel
      || (Array.isArray(enabledModels) && enabledModels.find(Boolean))
      || getDefaultModel(connection.provider)
      || getProviderModels(alias)[0]?.id;
    if (!model) {
      result = { valid: false, error: "No default model configured for this provider" };
    } else {
      const ping = await pingModelByKind(`${alias}/${model}`, "llm", new URL(request.url).origin);
      result = { valid: !!ping.ok, error: ping.error || null };
    }
  }

  const updateData = {
    testStatus: result.valid ? "active" : "error",
    lastError: result.valid ? null : result.error,
    lastErrorAt: result.valid ? null : new Date().toISOString(),
  };
  await updateProviderConnection(id, updateData);

  return {
    valid: result.valid,
    error: result.error,
    refreshed: false,
    latencyMs: Date.now() - startedAt,
    testedAt: new Date().toISOString(),
  };
}

// POST /api/providers/[id]/test - Test connection
export async function POST(request, { params }) {
  try {
    const { id } = await params;
    const result = process.env.NINEROUTER_RUNTIME === "cloudflare"
      ? await testCloudflareConnection(request, id)
      : await (await import("./testUtils.js")).testSingleConnection(id);

    if (result.error === "Connection not found") {
      return NextResponse.json({ error: "Connection not found" }, { status: 404 });
    }

    return NextResponse.json({
      valid: result.valid,
      error: result.error,
      refreshed: result.refreshed || false,
      latencyMs: result.latencyMs || 0,
      testedAt: result.testedAt || new Date().toISOString(),
    });
  } catch (error) {
    console.log("Error testing connection:", error);
    return NextResponse.json({ valid: false, error: error?.message || "Test failed" }, { status: 200 });
  }
}
