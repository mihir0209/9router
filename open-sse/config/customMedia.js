export const CUSTOM_MEDIA_KINDS = new Set(["embedding", "image", "video", "tts", "stt"]);

export const CUSTOM_MEDIA_PATHS = {
  embedding: "/embeddings",
  image: "/images/generations",
  video: "/videos/generations",
  tts: "/audio/speech",
  stt: "/audio/transcriptions",
};

const BLOCKED_HEADERS = new Set(["authorization", "content-length", "cookie", "host", "proxy-authorization"]);

export function sanitizeCustomHeaders(headers) {
  if (!headers || typeof headers !== "object" || Array.isArray(headers)) return {};
  const result = {};
  for (const [rawName, rawValue] of Object.entries(headers)) {
    const name = String(rawName).trim();
    const value = String(rawValue).trim();
    if (!name || !value || BLOCKED_HEADERS.has(name.toLowerCase()) || /[\r\n]/.test(name + value)) continue;
    result[name] = value;
  }
  return result;
}

export function resolveCustomMediaUrl(config, kind, action = null, requestId = null) {
  if (config?.mediaKind && config.mediaKind !== kind) {
    throw new Error(`Custom media provider is configured for ${config.mediaKind}, not ${kind}`);
  }
  const direct = String(config?.endpointUrl || "").trim();
  if (direct) {
    if (requestId) return `${direct.replace(/\/$/, "")}/${encodeURIComponent(requestId)}`;
    if (action && kind === "video" && action !== "generations") {
      return direct.replace(/\/generations\/?$/, `/${action}`);
    }
    return direct;
  }
  const base = String(config?.baseUrl || "").trim().replace(/\/$/, "");
  if (!base) throw new Error("Custom media provider URL is required");
  if (requestId) return `${base}/videos/${encodeURIComponent(requestId)}`;
  if (kind === "video" && action) return `${base}/videos/${action}`;
  return `${base}${CUSTOM_MEDIA_PATHS[kind]}`;
}

export function buildCustomMediaHeaders(config, credentials, contentType = "application/json") {
  const headers = { ...sanitizeCustomHeaders(config?.customHeaders) };
  if (contentType) headers["Content-Type"] = contentType;
  const token = credentials?.apiKey || credentials?.accessToken;
  switch (config?.authHeader) {
    case "none": break;
    case "x-api-key": if (token) headers["x-api-key"] = token; break;
    case "api-key": if (token) headers["api-key"] = token; break;
    default: if (token) headers.Authorization = `Bearer ${token}`;
  }
  return headers;
}
