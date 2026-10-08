import { NextResponse } from "next/server";
import { getUsageStats } from "@/lib/usageDb";
import { getApiKeys } from "@/lib/localDb";
import { hasValidAdminApiToken } from "@/lib/auth/adminApiToken";
import { coordinatorCall } from "@/lib/coordinator/client.js";

export const dynamic = "force-dynamic";

const VALID_PERIODS = new Set(["today", "24h", "7d", "30d", "60d", "all"]);

export async function GET(request) {
  try {
    const { searchParams } = new URL(request.url);
    const period = searchParams.get("period") || "7d";
    const keyId = searchParams.get("keyId") || null;
    const isAdminApi = await hasValidAdminApiToken(request);

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
        ...(!isAdminApi && { key: key.key }),
        maskedKey: key.key ? `${key.key.slice(0, 7)}...${key.key.slice(-4)}` : "",
        isActive: key.isActive,
        createdAt: key.createdAt,
        expiresAt: key.expiresAt,
        rateLimitRequests: key.rateLimitRequests,
        rateLimitWindowSeconds: key.rateLimitWindowSeconds,
        tokenLimitDaily: key.tokenLimitDaily,
        tokenLimitMonthly: key.tokenLimitMonthly,
        forceProvider: key.forceProvider || "",
        forceModel: key.forceModel || "",
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

    // Live rate-limit state per key (read-only peek: never burns budget).
    // Fail-open per key so one coordinator hiccup doesn't blank the tab.
    await Promise.all(keys.map(async (entry) => {
      const full = allKeys.find((k) => k.id === entry.id);
      const limit = Number(full?.rateLimitRequests) || 0;
      if (!limit) {
        entry.rateLimit = { limited: false };
        return;
      }
      try {
        const windowMs = (Number(full.rateLimitWindowSeconds) || 60) * 1000;
        const state = await coordinatorCall("peekRateLimit",
          { keyId: full.id, limit, windowMs, now: Date.now() },
          `tenant:${full.ownerRef || full.id}`);
        entry.rateLimit = {
          limited: true,
          limit,
          windowSeconds: Number(full.rateLimitWindowSeconds) || 60,
          remaining: state?.remaining ?? null,
          used: state?.count ?? null,
          resetAt: state?.resetAt ?? null,
        };
      } catch {
        entry.rateLimit = { limited: true, limit, remaining: null };
      }
    }));

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
