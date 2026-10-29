// Bounded body readers.
//
// `await request.json()` / `response.text()` buffer the whole body no matter how
// large. These helpers enforce a byte limit BEFORE the body is parsed: by
// Content-Length when the sender declares one, and by counting bytes while
// reading when it does not (chunked) so a lying or absent length cannot get
// past. A limit of 0 means "unlimited" and falls through to the native reader,
// so the default (non-Cloudflare) behaviour is byte-identical to before.

import { errorResponse } from "./error.js";
import { HTTP_STATUS } from "../config/runtimeConfig.js";

export class BodyTooLargeError extends Error {
  constructor(limitBytes, what = "request") {
    super(`${what} body exceeds the ${formatMb(limitBytes)} limit`);
    this.name = "BodyTooLargeError";
    this.status = HTTP_STATUS.PAYLOAD_TOO_LARGE;
    this.limitBytes = limitBytes;
    this.what = what;
  }
}

function formatMb(bytes) {
  const mb = bytes / (1024 * 1024);
  return `${Number.isInteger(mb) ? mb : mb.toFixed(2)} MB`;
}

/** Declared Content-Length, or null when absent / not a plain non-negative integer. */
export function declaredLength(headers) {
  const raw = headers?.get?.("content-length");
  if (raw == null || !/^\d+$/.test(raw.trim())) return null;
  const n = Number(raw);
  return Number.isSafeInteger(n) ? n : null;
}

/** OpenAI-style 413 response for a rejected request body. */
export function payloadTooLargeResponse(error) {
  return errorResponse(HTTP_STATUS.PAYLOAD_TOO_LARGE, error?.message || "Request body too large");
}

export function isBodyTooLarge(error) {
  return error instanceof BodyTooLargeError;
}

// Read at most `limit` bytes. Throws BodyTooLargeError as soon as the count passes it
// and cancels the rest of the stream so nothing past the limit is held.
async function readCounted(body, limit, what) {
  if (!body) return new Uint8Array(0);
  const reader = body.getReader();
  const chunks = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > limit) {
        reader.cancel().catch(() => {});
        throw new BodyTooLargeError(limit, what);
      }
      chunks.push(value);
    }
  } finally {
    try { reader.releaseLock(); } catch { /* cancelled / already released */ }
  }
  if (chunks.length === 1) return chunks[0];
  const out = new Uint8Array(total);
  let offset = 0;
  for (const c of chunks) { out.set(c, offset); offset += c.byteLength; }
  return out;
}

function rejectIfDeclaredTooLarge(source, limit, what) {
  const declared = declaredLength(source.headers);
  if (declared != null && declared > limit) {
    source.body?.cancel?.().catch?.(() => {});
    throw new BodyTooLargeError(limit, what);
  }
  return declared;
}

/**
 * Bytes of a Request/Response body, bounded by `limit` (0 = unlimited).
 * @returns {Promise<Uint8Array|ArrayBuffer>}
 */
export async function readBytesLimited(source, limit, what = "request") {
  if (!limit) return source.arrayBuffer();
  const declared = rejectIfDeclaredTooLarge(source, limit, what);
  if (declared != null) return source.arrayBuffer();   // runtime enforces the declared length
  return readCounted(source.body, limit, what);
}

export async function readTextLimited(source, limit, what = "request") {
  if (!limit) return source.text();
  const declared = rejectIfDeclaredTooLarge(source, limit, what);
  if (declared != null) return source.text();
  return new TextDecoder().decode(await readCounted(source.body, limit, what));
}

export async function readJsonLimited(source, limit, what = "request") {
  if (!limit) return source.json();
  const declared = rejectIfDeclaredTooLarge(source, limit, what);
  if (declared != null) return source.json();
  return JSON.parse(new TextDecoder().decode(await readCounted(source.body, limit, what)));
}

/**
 * Reject a request whose body is bigger than `limit` before it is parsed by
 * something we cannot bound (request.formData()): checks Content-Length and, for
 * a body without one, returns a request whose body errors past the limit.
 * Returns the request to read from.
 */
export function limitRequestBody(request, limit, what = "request") {
  if (!limit || !request.body) return request;
  const declared = rejectIfDeclaredTooLarge(request, limit, what);
  if (declared != null) return request;
  let total = 0;
  const counter = new TransformStream({
    transform(chunk, controller) {
      total += chunk.byteLength;
      if (total > limit) controller.error(new BodyTooLargeError(limit, what));
      else controller.enqueue(chunk);
    },
  });
  return new Request(request, { body: request.body.pipeThrough(counter), duplex: "half" });
}

/**
 * First `maxBytes` of a response body as text. The rest of the stream is
 * cancelled, so an enormous error page is never buffered. `truncated` tells the
 * caller the text is a prefix.
 */
export async function readTextPrefix(response, maxBytes) {
  if (!response?.body) {
    // Not a stream-backed Response (mocks, null body): nothing to bound.
    if (typeof response?.text === "function") {
      const text = await response.text();
      return text.length > maxBytes ? { text: text.slice(0, maxBytes), truncated: true } : { text, truncated: false };
    }
    return { text: "", truncated: false };
  }
  const reader = response.body.getReader();
  const chunks = [];
  let total = 0;
  let truncated = false;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (total + value.byteLength > maxBytes) {
        chunks.push(value.subarray(0, maxBytes - total));
        total = maxBytes;
        truncated = true;
        reader.cancel().catch(() => {});
        break;
      }
      chunks.push(value);
      total += value.byteLength;
    }
  } finally {
    try { reader.releaseLock(); } catch { /* cancelled */ }
  }
  const out = new Uint8Array(total);
  let offset = 0;
  for (const c of chunks) { out.set(c, offset); offset += c.byteLength; }
  return { text: new TextDecoder().decode(out), truncated };
}
