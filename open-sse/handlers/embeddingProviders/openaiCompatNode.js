// Custom node providers (openai-compatible-* / custom-embedding-*) — baseUrl from credentials
import createOpenAIEmbeddingAdapter from "./openai.js";
import { buildCustomMediaHeaders, resolveCustomMediaUrl } from "../../config/customMedia.js";

const baseAdapter = createOpenAIEmbeddingAdapter("openai");

const openaiCompatNode = {
  ...baseAdapter,
  buildUrl: (_model, creds) => {
    const rawBaseUrl = creds?.providerSpecificData?.baseUrl || "https://api.openai.com/v1";
    const baseUrl = rawBaseUrl.replace(/\/$/, "").replace(/\/embeddings$/, "");
    return `${baseUrl}/embeddings`;
  },
};

export const customMediaEmbedding = {
  ...baseAdapter,
  buildUrl: (_model, creds) => resolveCustomMediaUrl(creds?.providerSpecificData, "embedding"),
  buildHeaders: (creds) => buildCustomMediaHeaders(creds?.providerSpecificData, creds),
};

export default openaiCompatNode;
