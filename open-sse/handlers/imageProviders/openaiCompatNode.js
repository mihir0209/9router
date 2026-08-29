import { buildCustomMediaHeaders, resolveCustomMediaUrl } from "../../config/customMedia.js";

const OPENAI_IMAGE_FIELDS = [
  "prompt", "n", "quality", "response_format", "size", "style", "user",
  "background", "moderation", "output_compression", "output_format",
];

const openaiCompatNode = {
  buildUrl: (_model, credentials) => resolveCustomMediaUrl(credentials?.providerSpecificData, "image"),
  buildHeaders: (credentials) => buildCustomMediaHeaders(credentials?.providerSpecificData, credentials),
  buildBody: (model, body) => {
    const request = { model };
    for (const field of OPENAI_IMAGE_FIELDS) {
      if (body[field] !== undefined) request[field] = body[field];
    }
    return request;
  },
  normalize: (responseBody) => responseBody,
};

export default openaiCompatNode;
