import { PROVIDER_MODELS, PROVIDER_ID_TO_ALIAS } from "@/shared/constants/models";
import {
  isAnthropicCompatibleProvider,
  isOpenAICompatibleProvider,
} from "@/shared/constants/providers";
import { resolveKiroModels } from "open-sse/services/kiroModels.js";
import { resolveKimchiModels } from "open-sse/services/kimchiModels.js";
import { resolveQoderModels, routableQoderModels } from "open-sse/services/qoderModels.js";
import { resolveCopilotModels } from "open-sse/services/copilotModels.js";
import { resolveClinepassModels, resolveClineModels } from "open-sse/services/clinepassModels.js";
import { resolveGrokCliModels } from "open-sse/services/grokCliModels.js";
import { resolveCursorModels } from "open-sse/services/cursorModels.js";
import { resolveZedModels } from "open-sse/shared/zedAuth.js";
import { updateProviderCredentials } from "@/sse/services/tokenRefresh";
import { resolveConnectionProxyConfig } from "@/lib/network/connectionProxy";
import { getProviderModelCache } from "@/lib/db/repos/providerModelCacheRepo.js";

// Qoder shares one live resolver across intl (qoder) and CN (qoder-cn); the
// credentials carry the provider id so qoderModels picks the right region's
// catalog endpoint.
async function resolveQoderLiveModels(conn, provider, signal) {
  const result = await resolveQoderModels({
    provider,
    accessToken: conn.accessToken,
    // PAT (pt-...) connections keep the token in apiKey; without it the live
    // catalog silently fails and /v1/models falls back to the static list.
    apiKey: conn.apiKey,
    refreshToken: conn.refreshToken,
    email: conn.email,
    displayName: conn.displayName,
    providerSpecificData: conn.providerSpecificData || {},
  }, { signal });
  // Visible + hidden (enable:false) catalog keys — chat routes all of them.
  const models = routableQoderModels(result);
  if (!models.length) return null;
  return { models: models.map((m) => ({ id: m.id, name: m.name })) };
}

// Per-provider live model resolvers. Each receives a connection record and
// returns { models: [{ id, name? }, ...] } | null on failure.
// Adding a provider here makes /v1/models prefer the live catalog for it.
const LIVE_MODEL_RESOLVERS = {
  kiro: async (conn, signal) => {
    const result = await resolveKiroModels({
      accessToken: conn.accessToken,
      refreshToken: conn.refreshToken,
      providerSpecificData: conn.providerSpecificData || {}
    }, { log: console, signal });
    return result?.models?.length ? { models: result.models } : null;
  },
  qoder: async (conn, signal) => resolveQoderLiveModels(conn, "qoder", signal),
  "qoder-cn": async (conn, signal) => resolveQoderLiveModels(conn, "qoder-cn", signal),
  kimchi: async (conn, signal) => {
    const result = await resolveKimchiModels({
      accessToken: conn.accessToken,
      apiKey: conn.apiKey,
      providerSpecificData: conn.providerSpecificData || {}
    }, { log: console, signal });
    return result?.models?.length ? { models: result.models } : null;
  },
  github: async (conn, signal) => {
    const result = await resolveCopilotModels({
      accessToken: conn.accessToken,
      refreshToken: conn.refreshToken,
      providerSpecificData: conn.providerSpecificData || {}
    }, {
      log: console,
      signal,
      onCredentialsRefreshed: async (refreshed) => {
        await updateProviderCredentials(conn.id, {
          copilotToken: refreshed.copilotToken,
          copilotTokenExpiresAt: refreshed.copilotTokenExpiresAt,
          existingProviderSpecificData: conn.providerSpecificData || {},
        });
      },
    });
    return result?.models?.length ? { models: result.models } : null;
  },
  clinepass: async (conn) => {
    const result = await resolveClinepassModels({
      accessToken: conn.accessToken,
      apiKey: conn.apiKey,
    });
    return result?.models?.length ? { models: result.models } : null;
  },
  cline: async (conn) => {
    const result = await resolveClineModels({
      accessToken: conn.accessToken,
      apiKey: conn.apiKey,
    });
    return result?.models?.length ? { models: result.models } : null;
  },
  "grok-cli": async (conn, signal) => {
    const proxy = await resolveConnectionProxyConfig(conn.providerSpecificData || {});
    const result = await resolveGrokCliModels({
      ...conn,
      connectionId: conn.id,
    }, {
      log: console,
      signal,
      proxyOptions: {
        connectionProxyEnabled: proxy.connectionProxyEnabled === true,
        connectionProxyUrl: proxy.connectionProxyUrl || "",
        connectionNoProxy: proxy.connectionNoProxy || "",
        vercelRelayUrl: proxy.vercelRelayUrl || "",
        strictProxy: proxy.strictProxy === true,
      },
      onCredentialsRefreshed: async (refreshed) => {
        await updateProviderCredentials(conn.id, {
          ...refreshed,
          existingProviderSpecificData: conn.providerSpecificData || {},
        });
      },
    });
    return result?.models?.length ? { models: result.models } : null;
  },
  cursor: async (conn, signal) => {
    const result = await resolveCursorModels({
      accessToken: conn.accessToken,
      providerSpecificData: conn.providerSpecificData || {},
    }, { log: console, signal });
    return result?.models?.length ? { models: result.models } : null;
  },
  zed: async (conn, signal) => {
    const result = await resolveZedModels({
      accessToken: conn.accessToken,
      providerSpecificData: conn.providerSpecificData || {},
    }, { signal });
    if (!result?.models?.length) return null;
    return {
      models: result.models
        .filter((m) => !m.isDisabled)
        .map((m) => ({
          id: m.id,
          name: m.name,
          capabilities: m.supportsTools ? { tools: true } : undefined,
        })),
    };
  },
};

const parseOpenAIStyleModels = (data) => {
  if (Array.isArray(data)) return data;
  return data?.data || data?.models || data?.results || [];
};

// Header sent by fetchCompatibleModelIds to detect cross-instance /models fetches
// and break recursive loops between 9router instances connected to each other.
export const INTERNAL_MODELS_FETCH_HEADER = "x-9r-internal-models-fetch";

export async function fetchCompatibleModelIds(connection, signal) {
  if (!connection?.apiKey) return [];

  const baseUrl = typeof connection?.providerSpecificData?.baseUrl === "string"
    ? connection.providerSpecificData.baseUrl.trim().replace(/\/$/, "")
    : "";

  if (!baseUrl) return [];

  let url = `${baseUrl}/models`;
  const headers = {
    "Content-Type": "application/json",
  };

  if (isOpenAICompatibleProvider(connection.provider)) {
    headers.Authorization = `Bearer ${connection.apiKey}`;
  } else if (isAnthropicCompatibleProvider(connection.provider)) {
    if (url.endsWith("/messages/models")) {
      url = url.slice(0, -9);
    } else if (url.endsWith("/messages")) {
      url = `${url.slice(0, -9)}/models`;
    }
    headers["x-api-key"] = connection.apiKey;
    headers["anthropic-version"] = "2023-06-01";
    headers.Authorization = `Bearer ${connection.apiKey}`;
  } else {
    return [];
  }

  try {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 5000);
    const requestSignal = signal
      ? AbortSignal.any([signal, controller.signal])
      : controller.signal;
    const response = await fetch(url, {
      method: "GET",
      headers: { ...headers, [INTERNAL_MODELS_FETCH_HEADER]: "1" },
      cache: "no-store",
      signal: requestSignal,
    });
    clearTimeout(timeoutId);

    if (!response.ok) return [];

    const data = await response.json();
    const rawModels = parseOpenAIStyleModels(data);

    return Array.from(
      new Set(
        rawModels
          .map((model) => model?.id || model?.name || model?.model)
          .filter((modelId) => typeof modelId === "string" && modelId.trim() !== "")
      )
    );
  } catch {
    return [];
  }
}

/** Does this provider have a live discovery path at all? */
export function hasLiveModelSource(providerId, conn) {
  if (LIVE_MODEL_RESOLVERS[providerId]) return true;
  if (!conn?.apiKey) return false;
  return (
    isOpenAICompatibleProvider(providerId) ||
    isAnthropicCompatibleProvider(providerId)
  );
}

function staticEntriesFor(providerId) {
  const staticAlias = PROVIDER_ID_TO_ALIAS[providerId] || providerId;
  const providerModels = PROVIDER_MODELS[staticAlias] || [];
  return providerModels.map((m) => {
    const k = m?.kind || m?.type;
    return k ? { id: m.id, kind: k } : { id: m.id };
  });
}

function dedupeById(entries) {
  const seen = new Set();
  const out = [];
  for (const e of entries) {
    if (!e || typeof e.id !== "string" || e.id.trim() === "") continue;
    if (seen.has(e.id)) continue;
    seen.add(e.id);
    out.push(e);
  }
  return out;
}

/**
 * Resolve the model list for one provider connection.
 *
 * This is the single source of truth for "what models does this provider have",
 * shared by /v1/models and by the manual per-provider refresh. Keeping them on
 * one code path is what guarantees a refreshed cache produces the same list the
 * live path would have.
 *
 * @param {object} conn provider connection row
 * @param {string} providerId provider id (e.g. "kiro", "openai-compatible-<uuid>")
 * @param {object} opts
 * @param {boolean} opts.allowLive query the upstream (false = never leave the process)
 * @param {AbortSignal} [opts.signal]
 * @returns {Promise<{models: object[], source: "configured"|"live"|"cached"|"static", error?: string}>}
 */
export async function resolveProviderModelList(conn, providerId, { allowLive = false, signal } = {}) {
  // An explicitly configured model list always wins — it is a deliberate
  // admin choice, and it is how per-provider allow-lists work.
  const configuredMediaModels = conn?.providerSpecificData?.models;
  const enabledModels = Array.isArray(configuredMediaModels) && configuredMediaModels.length > 0
    ? configuredMediaModels
    : conn?.providerSpecificData?.enabledModels;
  const hasExplicitEnabledModels = Array.isArray(enabledModels) && enabledModels.length > 0;
  if (hasExplicitEnabledModels) {
    return { models: dedupeById(enabledModels.map((id) => ({ id }))), source: "configured" };
  }

  if (allowLive) {
    let rawModelIds = staticEntriesFor(providerId).map((e) => e.id);
    const isCompatibleProvider =
      isOpenAICompatibleProvider(providerId) || isAnthropicCompatibleProvider(providerId);

    if (isCompatibleProvider && rawModelIds.length === 0) {
      rawModelIds = await fetchCompatibleModelIds(conn, signal);
    }

    const liveResolver = LIVE_MODEL_RESOLVERS[providerId];
    if (liveResolver) {
      try {
        const live = await liveResolver(conn, signal);
        if (live?.models?.length) {
          return { models: dedupeById(live.models), source: "live" };
        }
      } catch (err) {
        // A failing live resolver must not empty the list; fall through to the
        // static ids below so the provider stays routable.
        console.log(`Live model fetch failed for ${providerId}: ${err?.message || err}`);
        return { models: dedupeById(rawModelIds.map((id) => ({ id }))), source: "static", error: err?.message || String(err) };
      }
    }
    return { models: dedupeById(rawModelIds.map((id) => ({ id }))), source: "live" };
  }

  // Cached mode: answer from the database. This is the whole point of the
  // setting — zero upstream calls, so /v1/models stays a single DB read.
  const cached = await getProviderModelCache(providerId);
  if (cached?.models?.length) {
    return { models: dedupeById(cached.models), source: "cached" };
  }
  // Never refreshed yet — the static catalog is still far better than an empty
  // list, and it costs no round trip.
  return { models: dedupeById(staticEntriesFor(providerId)), source: "static" };
}
