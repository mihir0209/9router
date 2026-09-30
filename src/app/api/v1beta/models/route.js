import { PROVIDER_MODELS, PROVIDER_ID_TO_ALIAS } from "@/shared/constants/models";
import { getProviderConnections } from "@/lib/localDb";
import {
  authenticateModelsRequest,
  modelsCacheId,
  modelsListHeaders,
  unauthorizedModelsResponse,
  withModelsCache,
} from "@/lib/modelCatalog/modelsListCache.js";

/**
 * Handle CORS preflight
 */
export async function OPTIONS() {
  return new Response(null, {
    headers: {
      "Access-Control-Allow-Origin": "*",
      "Access-Control-Allow-Methods": "GET, OPTIONS",
      "Access-Control-Allow-Headers": "*"
    }
  });
}

/**
 * GET /v1beta/models - Gemini compatible models list
 * Returns models in Gemini API format
 */
export async function GET(request) {
  try {
    // Same rule as /v1/models. This route reads every provider connection and
    // emits the Gemini-shaped catalogue, so leaving it open gave away the same
    // data through a second, cheaper-to-abuse spelling.
    const auth = await authenticateModelsRequest(request);
    if (!auth.ok) {
      return unauthorizedModelsResponse(auth.error, auth.status);
    }
    const keyRecord = auth.keyRecord;
    const cacheId = `${modelsCacheId(keyRecord)}|v1beta`;

    const { body, source } = await withModelsCache({
      request,
      cacheId,
      skipCache: false,
      build: async () => {
        const models = [];
        const seen = new Set();
        const connections = (await getProviderConnections()).filter((connection) => connection.isActive !== false);
        const activeAliases = new Set(
          connections.map((connection) => PROVIDER_ID_TO_ALIAS[connection.provider] || connection.provider)
        );

        function addModel({ name, displayName, description, methods = ["generateContent"] }) {
          if (seen.has(name)) return;
          seen.add(name);
          models.push({
            name,
            displayName,
            description,
            supportedGenerationMethods: methods,
            inputTokenLimit: 128000,
            outputTokenLimit: 8192,
          });
        }
    
        for (const [provider, providerModels] of Object.entries(PROVIDER_MODELS)) {
          if (!activeAliases.has(provider)) continue;
          for (const model of providerModels) {
            addModel({
              name: `models/${provider}/${model.id}`,
              displayName: model.name || model.id,
              description: `${provider} model: ${model.name || model.id}`,
            });

            if (provider === "gemini") {
              addModel({
                name: `models/${model.id}`,
                displayName: model.name || model.id,
                description: `Gemini model: ${model.name || model.id}`,
                methods: ["generateContent", "streamGenerateContent"],
              });
            }
          }
        }

      return { models };
      },
    });

    return new Response(body, {
      headers: modelsListHeaders({ "x-models-cache": source }),
    });
  } catch (error) {
    console.log("Error fetching models:", error);
    return Response.json(
      { error: { message: error.message } },
      { status: 500, headers: modelsListHeaders() }
    );
  }
}
