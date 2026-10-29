// Request/response size limits (NINEROUTER_MAX_BODY_MB & co) and the bounded readers.
import { describe, it, expect, afterEach, vi } from "vitest";
import {
  readJsonLimited, readTextLimited, readBytesLimited, readTextPrefix,
  limitRequestBody, BodyTooLargeError, declaredLength, payloadTooLargeResponse,
} from "../../open-sse/utils/bodyLimit.js";
import {
  getMaxJsonBodyBytes, getMaxUploadBodyBytes, getMaxResponseBodyBytes, getStreamCaptureMaxBytes,
  DEFAULT_CLOUDFLARE_MAX_BODY_MB,
} from "../../open-sse/config/bodyLimits.js";

const KEYS = ["NINEROUTER_RUNTIME", "NINEROUTER_MAX_BODY_MB", "NINEROUTER_MAX_UPLOAD_MB", "NINEROUTER_MAX_RESPONSE_MB", "NINEROUTER_STREAM_CAPTURE_MAX_BYTES"];
const saved = Object.fromEntries(KEYS.map((k) => [k, process.env[k]]));
afterEach(() => {
  for (const k of KEYS) { if (saved[k] === undefined) delete process.env[k]; else process.env[k] = saved[k]; }
});
const clear = () => KEYS.forEach((k) => delete process.env[k]);

const enc = new TextEncoder();

// A body that reports no Content-Length and counts how many chunks were pulled.
function chunkedBody(chunks, onPull) {
  let i = 0;
  return new ReadableStream({
    pull(controller) {
      if (i >= chunks.length) { controller.close(); return; }
      onPull?.(i);
      controller.enqueue(enc.encode(chunks[i++]));
    },
  });
}
const postStream = (body, headers = {}) => new Request("http://x.test/v1/chat/completions", { method: "POST", body, headers, duplex: "half" });

describe("limit resolution", () => {
  it("is off outside Cloudflare unless the env var is set", () => {
    clear();
    expect(getMaxJsonBodyBytes()).toBe(0);
    expect(getMaxUploadBodyBytes()).toBe(0);
    expect(getMaxResponseBodyBytes()).toBe(0);
    expect(getStreamCaptureMaxBytes()).toBe(0);
    process.env.NINEROUTER_MAX_BODY_MB = "2";
    expect(getMaxJsonBodyBytes()).toBe(2 * 1024 * 1024);
  });

  it("defaults on Cloudflare, env wins, 0 disables", () => {
    clear();
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    expect(getMaxJsonBodyBytes()).toBe(DEFAULT_CLOUDFLARE_MAX_BODY_MB * 1024 * 1024);
    expect(getMaxUploadBodyBytes()).toBeGreaterThan(getMaxJsonBodyBytes());
    expect(getStreamCaptureMaxBytes()).toBe(64 * 1024);
    process.env.NINEROUTER_MAX_BODY_MB = "1.5";
    expect(getMaxJsonBodyBytes()).toBe(1.5 * 1024 * 1024);
    process.env.NINEROUTER_MAX_BODY_MB = "0";
    expect(getMaxJsonBodyBytes()).toBe(0);
    process.env.NINEROUTER_MAX_BODY_MB = "junk";
    expect(getMaxJsonBodyBytes()).toBe(DEFAULT_CLOUDFLARE_MAX_BODY_MB * 1024 * 1024);
  });
});

describe("bounded readers", () => {
  it("limit 0 falls through to the native reader", async () => {
    const req = new Request("http://x.test", { method: "POST", body: '{"a":1}' });
    expect(await readJsonLimited(req, 0)).toEqual({ a: 1 });
  });

  it("rejects by Content-Length without reading the body", async () => {
    let pulled = 0;
    const req = postStream(chunkedBody(["x"], () => pulled++), { "content-length": "5000000" });
    await expect(readJsonLimited(req, 1024)).rejects.toBeInstanceOf(BodyTooLargeError);
    expect(pulled).toBe(0);
  });

  it("counts a chunked body and stops reading past the limit", async () => {
    let pulled = 0;
    const chunk = "x".repeat(1000);
    const req = postStream(chunkedBody(Array(1000).fill(chunk), () => pulled++));
    expect(req.headers.get("content-length")).toBeNull();
    await expect(readTextLimited(req, 4000)).rejects.toBeInstanceOf(BodyTooLargeError);
    expect(pulled).toBeLessThan(20);   // cancelled right after the limit, not drained
  });

  it("parses a chunked body that fits and keeps bytes intact", async () => {
    const req = postStream(chunkedBody(['{"a":', '"b\u00e9"}']));
    expect(await readJsonLimited(req, 1024)).toEqual({ a: "b\u00e9" });
    const bytes = await readBytesLimited(postStream(chunkedBody(["ab", "cd"])), 10);
    expect(new TextDecoder().decode(bytes)).toBe("abcd");
  });

  it("accepts a body exactly at the limit and rejects one byte over", async () => {
    expect(await readTextLimited(postStream(chunkedBody(["12345"])), 5)).toBe("12345");
    await expect(readTextLimited(postStream(chunkedBody(["123456"])), 5)).rejects.toBeInstanceOf(BodyTooLargeError);
  });

  it("limitRequestBody errors a chunked multipart body past the limit", async () => {
    const limited = limitRequestBody(postStream(chunkedBody(["x".repeat(600), "x".repeat(600)])), 1000);
    await expect(limited.arrayBuffer()).rejects.toBeInstanceOf(BodyTooLargeError);
    expect(() => limitRequestBody(postStream(chunkedBody(["x"]), { "content-length": "9999" }), 1000)).toThrow(BodyTooLargeError);
  });

  it("builds an OpenAI-style 413", async () => {
    const res = payloadTooLargeResponse(new BodyTooLargeError(1024 * 1024));
    expect(res.status).toBe(413);
    const body = await res.json();
    expect(body.error.type).toBe("invalid_request_error");
    expect(body.error.code).toBe("request_too_large");
    expect(body.error.message).toContain("1 MB");
  });

  it("declaredLength ignores junk", () => {
    expect(declaredLength(new Headers({ "content-length": "12" }))).toBe(12);
    expect(declaredLength(new Headers({ "content-length": "-1" }))).toBeNull();
    expect(declaredLength(new Headers())).toBeNull();
  });

  it("readTextPrefix returns the first N bytes and cancels the rest", async () => {
    let pulled = 0;
    const res = new Response(chunkedBody(Array(500).fill("y".repeat(1000)), () => pulled++));
    const { text, truncated } = await readTextPrefix(res, 2500);
    expect(truncated).toBe(true);
    expect(text.length).toBe(2500);
    expect(pulled).toBeLessThan(10);
    const small = await readTextPrefix(new Response("hello"), 100);
    expect(small).toEqual({ text: "hello", truncated: false });
  });
});

describe("handlers answer 413 before parsing", () => {
  it("handleChat rejects an oversized declared and an oversized chunked body", async () => {
    vi.resetModules();
    clear();
    process.env.NINEROUTER_MAX_BODY_MB = "1";
    const { handleChat } = await import("../../src/sse/handlers/chat.js");

    let pulled = 0;
    const declared = postStream(chunkedBody(["{}"], () => pulled++), { "content-length": String(3 * 1024 * 1024), "content-type": "application/json" });
    const r1 = await handleChat(declared);
    expect(r1.status).toBe(413);
    expect((await r1.json()).error.code).toBe("request_too_large");
    expect(pulled).toBe(0);

    const chunk = "a".repeat(256 * 1024);
    const big = postStream(chunkedBody([`{"model":"x","pad":"`, ...Array(8).fill(chunk), `"}`]), { "content-type": "application/json" });
    const r2 = await handleChat(big);
    expect(r2.status).toBe(413);
  });

  it("handleEmbeddings and handleImageGeneration also enforce it", async () => {
    vi.resetModules();
    clear();
    process.env.NINEROUTER_MAX_BODY_MB = "1";
    const { handleEmbeddings } = await import("../../src/sse/handlers/embeddings.js");
    const { handleImageGeneration } = await import("../../src/sse/handlers/imageGeneration.js");
    for (const handler of [handleEmbeddings, handleImageGeneration]) {
      const req = postStream(chunkedBody(["{}"]), { "content-length": String(2 * 1024 * 1024), "content-type": "application/json" });
      expect((await handler(req)).status).toBe(413);
    }
  });
});
