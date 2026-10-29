// Request/response size limits.
//
// A Worker isolate has 128 MB of memory shared by every concurrent request, and
// a JSON body is held several times over while it is handled (the raw bytes, the
// parsed object, the translated object, the re-serialised upstream body). So a
// limit has to sit well under 128 MB. On Cloudflare the limits are on by
// default; elsewhere they are off unless the env var is set, which keeps the
// Node build exactly as it was.
//
// This file must stay import-free: scripts/build-cloudflare.mjs imports it to
// stamp the same defaults into the worker guard.

export const DEFAULT_CLOUDFLARE_MAX_BODY_MB = 20;        // JSON bodies (chat, embeddings, image, tts, ...)
export const DEFAULT_CLOUDFLARE_MAX_UPLOAD_MB = 32;      // multipart / binary uploads (audio, video)
export const DEFAULT_CLOUDFLARE_MAX_RESPONSE_MB = 32;    // upstream bodies read whole (image, embeddings, non-stream)

export const BODY_LIMIT_ENV = {
  json: "NINEROUTER_MAX_BODY_MB",
  upload: "NINEROUTER_MAX_UPLOAD_MB",
  response: "NINEROUTER_MAX_RESPONSE_MB",
};

const BYTES_PER_MB = 1024 * 1024;

export function isCloudflareRuntime() {
  return process.env.NINEROUTER_RUNTIME === "cloudflare";
}

/**
 * Resolve a limit in bytes.
 * Returns 0 for "no limit". The env var is a number of megabytes; 0 or a
 * negative value disables the limit explicitly, an unparsable value is ignored.
 */
function resolveLimitBytes(envName, cloudflareDefaultMb) {
  const raw = process.env[envName];
  if (raw != null && raw !== "") {
    const mb = Number(raw);
    if (Number.isFinite(mb)) return mb > 0 ? Math.floor(mb * BYTES_PER_MB) : 0;
  }
  return isCloudflareRuntime() ? cloudflareDefaultMb * BYTES_PER_MB : 0;
}

export const getMaxJsonBodyBytes = () => resolveLimitBytes(BODY_LIMIT_ENV.json, DEFAULT_CLOUDFLARE_MAX_BODY_MB);
export const getMaxUploadBodyBytes = () => resolveLimitBytes(BODY_LIMIT_ENV.upload, DEFAULT_CLOUDFLARE_MAX_UPLOAD_MB);
export const getMaxResponseBodyBytes = () => resolveLimitBytes(BODY_LIMIT_ENV.response, DEFAULT_CLOUDFLARE_MAX_RESPONSE_MB);

// Retained text for request details / onStreamComplete. Per field (content, thinking).
// 0 = keep everything. Env: NINEROUTER_STREAM_CAPTURE_MAX_BYTES.
export const DEFAULT_CLOUDFLARE_STREAM_CAPTURE_MAX_BYTES = 64 * 1024;
export function getStreamCaptureMaxBytes() {
  const raw = process.env.NINEROUTER_STREAM_CAPTURE_MAX_BYTES;
  if (raw != null && raw !== "") {
    const n = Number(raw);
    if (Number.isFinite(n)) return n > 0 ? Math.floor(n) : 0;
  }
  return isCloudflareRuntime() ? DEFAULT_CLOUDFLARE_STREAM_CAPTURE_MAX_BYTES : 0;
}

export const STREAM_CAPTURE_TRUNCATION_MARKER = "\n[...truncated: stream capture limit reached]";

// Most bytes the empty-failover peek holds while waiting for a content frame.
export const EMPTY_PEEK_MAX_BYTES_DEFAULT = 256 * 1024;
export function getEmptyPeekMaxBytes() {
  const raw = process.env.NINEROUTER_EMPTY_PEEK_MAX_BYTES;
  if (raw != null && raw !== "") {
    const n = Number(raw);
    if (Number.isFinite(n)) return n > 0 ? Math.floor(n) : 0;
  }
  return EMPTY_PEEK_MAX_BYTES_DEFAULT;
}

// Most bytes of an upstream error body that is read for matching / messages.
export const UPSTREAM_ERROR_READ_MAX_BYTES = 64 * 1024;

// Largest 200 JSON reply the combo zero-content check will parse. Larger ones plainly have content.
export const COMBO_PEEK_JSON_MAX_BYTES = 256 * 1024;
