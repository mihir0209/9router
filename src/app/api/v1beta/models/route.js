import { PROVIDER_MODELS, PROVIDER_ID_TO_ALIAS } from "@/shared/constants/models";
import { getProviderConnections } from "@/lib/localDb";
import {
  authenticateModelsRequest,
  coalesceModelsBuild,
  modelsListHeaders,
  unauthorizedModelsResponse,
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

    // Live per request, like the other catalogue routes.
    const { value: models, coalesced } = await coalesceModelsBuild(
      `v1beta:${keyRecord.id}`,
      async () => {
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

      return models;
      });

    return new Response(JSON.stringify({ models }), {
      headers: modelsListHeaders({ "x-models-build": coalesced ? "coalesced" : "fresh" }),
    });
  } catch (error) {
    console.log("Error fetching models:", error);
    return Response.json(
      { error: { message: error.message } },
      { status: 500, headers: modelsListHeaders() }
    );
  }
}
