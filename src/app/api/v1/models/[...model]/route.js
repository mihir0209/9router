import { buildModelsList } from "../route.js";
import {
  authenticateModelsRequest,
  modelsCacheId,
  modelsListHeaders,
  parseModelList,
  unauthorizedModelsResponse,
  withModelsCache,
} from "@/lib/modelCatalog/modelsListCache.js";

// URL slug → service kind(s). `web` covers both webSearch and webFetch.
const KIND_SLUG_MAP = {
  "image": ["image"],
  "tts": ["tts"],
  "stt": ["stt"],
  "embedding": ["embedding"],
  "image-to-text": ["imageToText"],
  "web": ["webSearch", "webFetch"],
};

const LLM_KIND = "llm";

export async function OPTIONS() {
  return new Response(null, {
    headers: {
      "Access-Control-Allow-Origin": "*",
      "Access-Control-Allow-Methods": "GET, OPTIONS",
      "Access-Control-Allow-Headers": "*",
    },
  });
}

function json(data, options = {}) {
  return Response.json(data, {
    ...options,
    headers: {
      ...modelsListHeaders(),
      ...options.headers,
    },
  });
}

/**
 * GET /v1/models/{kind} - OpenAI-compatible models list filtered by capability.
 * GET /v1/models/{provider}/{model} - OpenAI-compatible single model lookup.
 * Supported kinds: image, tts, stt, embedding, image-to-text, web.
 */
export async function GET(request, { params }) {
  try {
    // Same rule as /v1/models: this route builds the same catalogue and is just
    // as expensive, so it authenticates and caches identically. Guarding only
    // the list route would leave the cost on the /{kind} spelling.
    const auth = await authenticateModelsRequest(request);
    if (!auth.ok) {
      return unauthorizedModelsResponse(auth.error, auth.status);
    }
    const keyRecord = auth.keyRecord;
    const allowedModels = parseModelList(keyRecord.allowedModels);

    const { model } = await params;
    const path = Array.isArray(model) ? model : [model];
    const identifier = path.filter(Boolean).join("/");
    const kindFilter = path.length === 1 ? KIND_SLUG_MAP[identifier] : null;

    if (kindFilter) {
      // The cache id carries the key, but not the kind, so the kind is part of
      // the identity too. Otherwise an image-model list would be served to
      // someone asking for embeddings.
      const cacheId = `${modelsCacheId(keyRecord)}|${identifier}`;
      const { body, source } = await withModelsCache({
        request,
        cacheId,
        skipCache: false,
        build: async () => ({ object: "list", data: await buildModelsList(kindFilter, { allowedModels }) }),
      });
      return new Response(body, { headers: modelsListHeaders({ "x-models-cache": source }) });
    }

    // A single-model lookup is answered from the cached LLM list rather than a
    // cached per-model verdict. Caching the verdict would keep answering 404
    // for up to 10 minutes after the model is added, and would rebuild the
    // whole list for every distinct model name asked for.
    const { body } = await withModelsCache({
      request,
      cacheId: `${modelsCacheId(keyRecord)}|llm`,
      skipCache: false,
      build: async () => ({
        object: "list",
        data: await buildModelsList([LLM_KIND], { allowedModels }),
      }),
    });

    const matchedModel = JSON.parse(body).data.find((candidate) => candidate.id === identifier);
    if (!matchedModel) {
      return json(
        {
          error: {
            message: `The model '${identifier}' does not exist or you do not have access to it.`,
            type: "invalid_request_error",
            code: "model_not_found",
          },
        },
        { status: 404 }
      );
    }
    return json(matchedModel);
  } catch (error) {
    console.log("Error fetching model:", error);
    return json(
      { error: { message: error.message, type: "server_error" } },
      { status: 500 }
    );
  }
}
