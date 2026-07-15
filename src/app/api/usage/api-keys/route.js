import { NextResponse } from "next/server";
import { getUsageStats } from "@/lib/usageDb";
import { getApiKeys } from "@/lib/localDb";

export const dynamic = "force-dynamic";

const VALID_PERIODS = new Set(["today", "24h", "7d", "30d", "60d", "all"]);

export async function GET(request) {
  try {
    const { searchParams } = new URL(request.url);
    const period = searchParams.get("period") || "7d";
    const keyId = searchParams.get("keyId") || null;

    if (!VALID_PERIODS.has(period)) {
      return NextResponse.json({ error: "Invalid period" }, { status: 400 });
    }

    const stats = await getUsageStats(period);
    const allKeys = await getApiKeys();

    // Build per-key summary
    const keyUsageMap = {};
    for (const key of allKeys) {
      keyUsageMap[key.key] = {
        id: key.id,
        name: key.name,
        key: key.key,
        maskedKey: key.key ? `${key.key.slice(0, 7)}...${key.key.slice(-4)}` : "",
        isActive: key.isActive,
        createdAt: key.createdAt,
        expiresAt: key.expiresAt,
        rateLimitRequests: key.rateLimitRequests,
        rateLimitWindowSeconds: key.rateLimitWindowSeconds,
        tokenLimitDaily: key.tokenLimitDaily,
        tokenLimitMonthly: key.tokenLimitMonthly,
        allowedModels: key.allowedModels,
        blockedModels: key.blockedModels,
        allowedIPs: key.allowedIPs,
        blockedIPs: key.blockedIPs,
        description: key.description,
        usage: {
          requests: 0,
          promptTokens: 0,
          completionTokens: 0,
          cachedTokens: 0,
          cost: 0,
          models: {},
          providers: {},
        },
      };
    }

    // Aggregate usage from stats.byApiKey
    for (const [akKey, akData] of Object.entries(stats.byApiKey || {})) {
      // Extract the raw API key from the composite key (format: "masked|model|provider")
      const parts = akKey.split("|");
      const maskedKey = parts[0] || "";

      // Find the matching key
      const matchedKey = allKeys.find(k => {
        const masked = k.key ? `${k.key.slice(0, 7)}...${k.key.slice(-4)}` : "";
        return masked === maskedKey || k.key === maskedKey;
      });

      if (matchedKey && keyUsageMap[matchedKey.key]) {
        const usage = keyUsageMap[matchedKey.key].usage;
        usage.requests += akData.requests || 0;
        usage.promptTokens += akData.promptTokens || 0;
        usage.completionTokens += akData.completionTokens || 0;
        usage.cachedTokens += akData.cachedTokens || 0;
        usage.cost += akData.cost || 0;

        // Track by model
        const model = akData.rawModel || "";
        if (model) {
          if (!usage.models[model]) {
            usage.models[model] = { requests: 0, promptTokens: 0, completionTokens: 0, cost: 0 };
          }
          usage.models[model].requests += akData.requests || 0;
          usage.models[model].promptTokens += akData.promptTokens || 0;
          usage.models[model].completionTokens += akData.completionTokens || 0;
          usage.models[model].cost += akData.cost || 0;
        }

        // Track by provider
        const provider = akData.provider || "";
        if (provider) {
          if (!usage.providers[provider]) {
            usage.providers[provider] = { requests: 0, promptTokens: 0, completionTokens: 0, cost: 0 };
          }
          usage.providers[provider].requests += akData.requests || 0;
          usage.providers[provider].promptTokens += akData.promptTokens || 0;
          usage.providers[provider].completionTokens += akData.completionTokens || 0;
          usage.providers[provider].cost += akData.cost || 0;
        }
      }
    }

    let keys = Object.values(keyUsageMap);

    // Filter by specific key if requested
    if (keyId) {
      keys = keys.filter(k => k.id === keyId);
    }

    // Sort by total requests descending
    keys.sort((a, b) => b.usage.requests - a.usage.requests);

    // Summary totals
    const summary = {
      totalKeys: allKeys.length,
      activeKeys: allKeys.filter(k => k.isActive).length,
      totalRequests: keys.reduce((sum, k) => sum + k.usage.requests, 0),
      totalTokens: keys.reduce((sum, k) => sum + k.usage.promptTokens + k.usage.completionTokens, 0),
      totalCost: keys.reduce((sum, k) => sum + k.usage.cost, 0),
    };

    return NextResponse.json({ summary, keys, period });
  } catch (error) {
    console.error("[API] Failed to get API key usage:", error);
    return NextResponse.json({ error: "Failed to fetch API key usage" }, { status: 500 });
  }
}
