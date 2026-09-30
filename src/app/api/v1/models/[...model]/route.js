import { buildModelsList } from "../route.js";
import {
  authenticateModelsRequest,
  coalesceModelsBuild,
  modelsListHeaders,
  parseModelList,
  unauthorizedModelsResponse,
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
      // Built per request so the list is live. The coalescing key carries the
      // key and the kind, so an image list is never handed to an embeddings
      // caller and one caller's allow-list is never handed to another's.
      const data = await coalesceModelsBuild(
        `${identifier}:${keyRecord.id}:${allowedModels ? allowedModels.join(",") : "*"}`,
        () => buildModelsList(kindFilter, { allowedModels })
      );
      return new Response(JSON.stringify({ object: "list", data }), {
        headers: modelsListHeaders(),
      });
    }

    // A single-model lookup builds the LLM list for this caller and reads the one
    // entry, rather than a cached per-model verdict: caching the verdict would
    // keep answering 404 for up to the TTL after a model is added.
    const data = await coalesceModelsBuild(
      `llm:${keyRecord.id}:${allowedModels ? allowedModels.join(",") : "*"}`,
      () => buildModelsList([LLM_KIND], { allowedModels })
    );

    const matchedModel = data.find((candidate) => candidate.id === identifier);
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
