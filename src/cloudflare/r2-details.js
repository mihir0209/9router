/**
 * Cloudflare R2 offloading for heavy `requestDetails` payloads.
 *
 * When an R2 bucket binding (`REQUEST_DETAILS_BUCKET` or `R2_BUCKET`) is
 * available, large or body-bearing request/response payloads are stored in R2
 * under `request-details/<YYYY-MM-DD>/<id>.json` and D1 keeps only compact
 * metadata + `r2Key`. Reads transparently hydrate from R2 with graceful
 * fallback when R2 is unavailable or an object is missing.
 */

const DEFAULT_OFFLOAD_THRESHOLD_BYTES = 1024;
const BODY_FIELDS = ["request", "providerRequest", "providerResponse", "response"];

let injectedBucket = null;

/**
 * Override the R2 bucket binding (intended for unit tests and benchmarks).
 * Pass `null` to clear the override.
 */
export function setR2Bucket(bucket) {
  injectedBucket = bucket || null;
}

/**
 * Resolve the configured offload threshold in bytes.
 */
export function getOffloadThresholdBytes() {
  try {
    const raw = typeof process !== "undefined" ? process.env?.NINEROUTER_R2_OFFLOAD_THRESHOLD_BYTES : undefined;
    if (raw !== undefined) {
      const parsed = Number(raw);
      if (Number.isFinite(parsed) && parsed >= 0) return parsed;
    }
  } catch {}
  return DEFAULT_OFFLOAD_THRESHOLD_BYTES;
}

function pickBucketFromEnv(env) {
  if (!env || typeof env !== "object") return null;
  return env.REQUEST_DETAILS_BUCKET || env.R2_BUCKET || null;
}

/**
 * Resolve an R2 bucket binding from explicit options, test injection,
 * global Cloudflare context, or `@opennextjs/cloudflare`.
 */
export async function resolveR2Bucket(options = {}) {
  if (options?.bucket) return options.bucket;
  if (injectedBucket) return injectedBucket;

  const fromExplicitEnv = pickBucketFromEnv(options?.env);
  if (fromExplicitEnv) return fromExplicitEnv;

  try {
    const cfContext = globalThis[Symbol.for("__cloudflare-context__")];
    const fromGlobal = pickBucketFromEnv(cfContext?.env);
    if (fromGlobal) return fromGlobal;
  } catch {}

  try {
    const { getCloudflareContext } = await import("@opennextjs/cloudflare");
    const ctx = await getCloudflareContext({ async: true });
    const fromCf = pickBucketFromEnv(ctx?.env);
    if (fromCf) return fromCf;
  } catch {
    // Not running inside OpenNext / Cloudflare Workers
  }

  return null;
}

function utf8ByteLength(str) {
  return new TextEncoder().encode(str).byteLength;
}

function formatUtcDate(timestamp) {
  if (timestamp) {
    const d = new Date(timestamp);
    if (!Number.isNaN(d.getTime())) {
      return d.toISOString().slice(0, 10);
    }
  }
  return new Date().toISOString().slice(0, 10);
}

/**
 * Build the canonical R2 object key: `request-details/<YYYY-MM-DD>/<id>.json`.
 */
export function buildR2DetailKey(id, timestamp) {
  const datePart = formatUtcDate(timestamp);
  const safeId = String(id || "unknown")
    .replace(/^\/+/, "")
    .replace(/\.\./g, "_")
    .replace(/[?#]/g, "_");
  return `request-details/${datePart}/${safeId}.json`;
}

function hasNonEmptyBody(record) {
  for (const field of BODY_FIELDS) {
    const val = record?.[field];
    if (val && typeof val === "object" && Object.keys(val).length > 0) {
      return true;
    }
    if (typeof val === "string" && val.length > 0) {
      return true;
    }
  }
  return false;
}

/**
 * Determine whether a full `requestDetails` record should be offloaded to R2.
 */
export function shouldOffloadToR2(record, thresholdBytes = getOffloadThresholdBytes()) {
  if (!record || typeof record !== "object") return false;
  if (hasNonEmptyBody(record)) return true;
  const serialized = JSON.stringify(record);
  return utf8ByteLength(serialized) > thresholdBytes;
}

/**
 * Strip heavy request/response blobs and return compact D1 metadata + `r2Key`.
 */
export function buildCompactD1Record(fullRecord, r2Key, r2Bytes) {
  return {
    id: fullRecord.id,
    provider: fullRecord.provider || null,
    model: fullRecord.model || null,
    connectionId: fullRecord.connectionId || null,
    timestamp: fullRecord.timestamp,
    status: fullRecord.status || null,
    latency: fullRecord.latency || {},
    tokens: fullRecord.tokens || {},
    r2Key,
    ...(typeof r2Bytes === "number" ? { r2Bytes } : {}),
  };
}

/**
 * Offload a `requestDetails` record to R2 if a bucket is available and the
 * record qualifies. Returns `{ record, offloaded, r2Key, originalBytes, compactBytes }`.
 */
export async function offloadRequestDetailToR2(fullRecord, options = {}) {
  const bucket = await resolveR2Bucket(options);
  if (!bucket || typeof bucket.put !== "function") {
    return { record: fullRecord, offloaded: false, r2Key: null };
  }

  const thresholdBytes = options.thresholdBytes ?? getOffloadThresholdBytes();
  if (!shouldOffloadToR2(fullRecord, thresholdBytes)) {
    return { record: fullRecord, offloaded: false, r2Key: null };
  }

  const r2Key = buildR2DetailKey(fullRecord.id, fullRecord.timestamp);
  const payloadJson = JSON.stringify(fullRecord);
  const originalBytes = utf8ByteLength(payloadJson);

  await bucket.put(r2Key, payloadJson, {
    httpMetadata: { contentType: "application/json" },
    customMetadata: {
      id: String(fullRecord.id || ""),
      provider: String(fullRecord.provider || ""),
      model: String(fullRecord.model || ""),
      timestamp: String(fullRecord.timestamp || ""),
    },
  });

  const compactRecord = buildCompactD1Record(fullRecord, r2Key, originalBytes);
  const compactBytes = utf8ByteLength(JSON.stringify(compactRecord));

  return {
    record: compactRecord,
    offloaded: true,
    r2Key,
    originalBytes,
    compactBytes,
  };
}

async function readR2ObjectJson(obj) {
  if (!obj) return null;
  if (typeof obj.json === "function") return await obj.json();
  if (typeof obj.text === "function") return JSON.parse(await obj.text());
  if (typeof obj === "string") return JSON.parse(obj);
  return null;
}

/**
 * Hydrate a single D1 `requestDetails` record from R2 if it contains `r2Key`.
 * Falls back gracefully to the compact D1 record if R2 is unavailable, the
 * object is missing, or reading/parsing fails.
 */
export async function hydrateRequestDetailFromR2(record, options = {}) {
  if (!record || typeof record !== "object" || !record.r2Key) {
    return record;
  }

  try {
    const bucket = await resolveR2Bucket(options);
    if (!bucket || typeof bucket.get !== "function") {
      return record;
    }

    const obj = await bucket.get(record.r2Key);
    if (!obj) {
      return record;
    }

    const hydrated = await readR2ObjectJson(obj);
    if (!hydrated || typeof hydrated !== "object") {
      return record;
    }

    return {
      ...record,
      ...hydrated,
      r2Key: record.r2Key,
    };
  } catch (err) {
    console.warn(`[r2-details] Failed to hydrate ${record.r2Key}:`, err?.message || err);
    return record;
  }
}

/**
 * Hydrate an array of `requestDetails` records in parallel.
 */
export async function hydrateRequestDetailsFromR2(records, options = {}) {
  if (!Array.isArray(records) || records.length === 0) return records;
  const hasOffloaded = records.some((r) => r && typeof r === "object" && r.r2Key);
  if (!hasOffloaded) return records;

  const bucket = await resolveR2Bucket(options);
  if (!bucket || typeof bucket.get !== "function") return records;

  return Promise.all(records.map((r) => hydrateRequestDetailFromR2(r, { ...options, bucket })));
}
