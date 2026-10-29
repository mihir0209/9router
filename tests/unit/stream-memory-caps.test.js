// Memory caps on the streaming path: retained stream text (C), the empty-failover
// peek (D) and the upstream error read (E).
import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";

vi.mock("@/lib/usageDb.js", () => ({
  saveRequestDetail: vi.fn(async () => {}),
  saveRequestUsage: vi.fn(async () => {}),
  appendRequestLog: vi.fn(async () => {}),
  trackPendingRequest: vi.fn(),
}));

const fetchMock = vi.fn();
vi.mock("../../open-sse/utils/proxyFetch.js", () => ({
  proxyAwareFetch: (...args) => fetchMock(...args),
}));

const { createPassthroughStreamWithLogger, createTextCapture } = await import("../../open-sse/utils/stream.js");
const { peekStreamForContent, reconstructStream } = await import("../../open-sse/services/emptyFailover.js");
const { BaseExecutor } = await import("../../open-sse/executors/base.js");
const { STREAM_CAPTURE_TRUNCATION_MARKER, UPSTREAM_ERROR_READ_MAX_BYTES } = await import("../../open-sse/config/bodyLimits.js");

const enc = new TextEncoder();
const dec = new TextDecoder();

const sse = (obj) => `data: ${JSON.stringify(obj)}\n\n`;
const contentFrame = (text) => sse({ id: "x", choices: [{ index: 0, delta: { content: text }, finish_reason: null }] });
const thinkingFrame = (text) => sse({ id: "x", choices: [{ index: 0, delta: { reasoning_content: text }, finish_reason: null }] });

async function drain(readable) {
  const reader = readable.getReader();
  let n = 0;
  for (;;) { const { done, value } = await reader.read(); if (done) break; n += value.byteLength; }
  return n;
}

async function runPassthrough(frames) {
  let complete = null;
  const transform = createPassthroughStreamWithLogger("openai", null, "m", "conn", { messages: [{ role: "user", content: "hi" }] },
    (contentObj, usage) => { complete = { contentObj, usage }; });
  const source = new ReadableStream({
    start(c) { for (const f of frames) c.enqueue(enc.encode(f)); c.close(); },
  });
  const bytesOut = await drain(source.pipeThrough(transform));
  return { complete, bytesOut };
}

const ENV = ["NINEROUTER_RUNTIME", "NINEROUTER_STREAM_CAPTURE_MAX_BYTES", "NINEROUTER_EMPTY_PEEK_MAX_BYTES"];
const saved = Object.fromEntries(ENV.map((k) => [k, process.env[k]]));
beforeEach(() => ENV.forEach((k) => delete process.env[k]));
afterEach(() => { for (const k of ENV) { if (saved[k] === undefined) delete process.env[k]; else process.env[k] = saved[k]; } });

describe("createTextCapture", () => {
  it("is unbounded with max 0 and truncates with a marker otherwise", () => {
    const open = createTextCapture(0);
    open.add("abc"); open.add("def");
    expect(open.text()).toBe("abcdef");
    const capped = createTextCapture(5);
    capped.add("abc"); capped.add("defgh"); capped.add("ignored");
    expect(capped.truncated).toBe(true);
    expect(capped.text()).toBe("abcde" + STREAM_CAPTURE_TRUNCATION_MARKER);
    const exact = createTextCapture(3);
    exact.add("abc");
    expect(exact.truncated).toBe(false);
    expect(exact.text()).toBe("abc");
  });
});

describe("stream capture cap", () => {
  const frames = [
    ...Array.from({ length: 200 }, (_, i) => thinkingFrame("t".repeat(100) + i)),
    ...Array.from({ length: 200 }, (_, i) => contentFrame("c".repeat(100) + i)),
    "data: [DONE]\n\n",
  ];

  it("keeps everything by default (Node)", async () => {
    const { complete } = await runPassthrough(frames);
    expect(complete.contentObj.content.length).toBeGreaterThan(20000);
    expect(complete.contentObj.content).not.toContain("truncated");
    expect(complete.contentObj.thinking.length).toBeGreaterThan(20000);
  });

  it("caps retained text per field but forwards every byte and keeps the usage estimate exact", async () => {
    const full = await runPassthrough(frames);
    process.env.NINEROUTER_STREAM_CAPTURE_MAX_BYTES = "2048";
    const capped = await runPassthrough(frames);
    expect(capped.bytesOut).toBe(full.bytesOut);
    expect(capped.complete.contentObj.content.length).toBe(2048 + STREAM_CAPTURE_TRUNCATION_MARKER.length);
    expect(capped.complete.contentObj.thinking.length).toBe(2048 + STREAM_CAPTURE_TRUNCATION_MARKER.length);
    expect(capped.complete.contentObj.content.endsWith(STREAM_CAPTURE_TRUNCATION_MARKER)).toBe(true);
    // usage is estimated from totalContentLength, which the cap must not touch
    expect(capped.complete.usage).toEqual(full.complete.usage);
  });

  it("defaults to 64 KB on the Cloudflare runtime", async () => {
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    const many = Array.from({ length: 1500 }, (_, i) => contentFrame("c".repeat(100) + i));
    const { complete } = await runPassthrough([...many, "data: [DONE]\n\n"]);
    expect(complete.contentObj.content.length).toBe(64 * 1024 + STREAM_CAPTURE_TRUNCATION_MARKER.length);
  });
});

describe("peekStreamForContent byte cap", () => {
  const ROLE = sse({ id: "x", choices: [{ index: 0, delta: { role: "assistant" }, finish_reason: null }] });

  function slowStream(frames, state) {
    let i = 0;
    return new ReadableStream({
      pull(c) {
        if (i >= frames.length) { c.close(); return; }
        state.pulled = i + 1;
        c.enqueue(enc.encode(frames[i++]));
      },
    });
  }

  it("commits fail-open once the held head reaches maxBytes, without dropping a chunk", async () => {
    const frames = [...Array(200).fill(ROLE), contentFrame("late content"), "data: [DONE]\n\n"];
    const state = {};
    const peeked = await peekStreamForContent(slowStream(frames, state), { maxBytes: 1000 });
    expect(peeked.outcome).toBe("content");
    expect(peeked.byteCapped).toBe(true);
    expect(peeked.timedOut).toBe(false);
    expect(peeked.pendingRead).toBeNull();
    const held = peeked.head.reduce((n, c) => n + c.byteLength, 0);
    expect(held).toBeGreaterThanOrEqual(1000);
    expect(held).toBeLessThan(1000 + enc.encode(ROLE).byteLength);
    expect(state.pulled).toBeLessThan(frames.length);   // stopped reading, not drained

    // The replay still carries every frame in order.
    const replay = reconstructStream(peeked.head, peeked.reader, peeked.pendingRead);
    const text = dec.decode(await new Response(replay).arrayBuffer());
    expect(text).toBe(frames.join(""));
  });

  it("is a no-op below the cap and when disabled", async () => {
    const frames = [ROLE, ROLE, "data: [DONE]\n\n"];
    const capped = await peekStreamForContent(slowStream(frames, {}), { maxBytes: 1_000_000 });
    expect(capped.outcome).toBe("empty");
    expect(capped.byteCapped).toBeUndefined();
    const off = await peekStreamForContent(slowStream([...Array(50).fill(ROLE), "data: [DONE]\n\n"], {}), { maxBytes: 0 });
    expect(off.outcome).toBe("empty");
  });

  it("still returns on the first content frame well under the cap", async () => {
    const peeked = await peekStreamForContent(slowStream([ROLE, contentFrame("hi")], {}), { maxBytes: 100000 });
    expect(peeked.outcome).toBe("content");
    expect(peeked.byteCapped).toBeUndefined();
  });
});

describe("upstream error body read is bounded", () => {
  it("reads at most the cap for matching, cancels the rest, and leaves the body readable", async () => {
    const CHUNK = 16 * 1024;
    const TOTAL_CHUNKS = 400;   // 6.4 MB error page
    let pulled = 0;
    const makeBody = () => new ReadableStream({
      pull(c) {
        if (pulled >= TOTAL_CHUNKS) { c.close(); return; }
        pulled++;
        c.enqueue(new Uint8Array(CHUNK).fill(65));
      },
    });
    fetchMock.mockReset();
    fetchMock.mockResolvedValue(new Response(makeBody(), { status: 400, headers: { "content-type": "text/html" } }));

    const ex = new BaseExecutor("test-prov", { baseUrl: "https://x/api", retry: { 502: { attempts: 0 }, 503: { attempts: 0 }, 504: { attempts: 0 }, 429: { attempts: 0 } } });
    const out = await ex.execute({ model: "m", body: {}, stream: false, credentials: { apiKey: "k" } });
    expect(out.response.status).toBe(400);
    // pulled only about the cap (plus the stream's own read-ahead), not 6.4 MB
    expect(pulled * CHUNK).toBeLessThan(UPSTREAM_ERROR_READ_MAX_BYTES + 8 * CHUNK);
    const rest = await out.response.arrayBuffer();
    expect(rest.byteLength).toBe(TOTAL_CHUNKS * CHUNK);
  });

  it("still matches error text inside the first N KB", async () => {
    fetchMock.mockReset();
    const { registerRetryPlugin, clearRetryPlugins } = await import("../../open-sse/retry/index.js");
    registerRetryPlugin("cap-prov", {
      errorRules: [{ text: "please retry", retryHttp: true, attempts: 1, delayMs: 0 }],
      httpRetry: { 502: { attempts: 0 }, 503: { attempts: 0 }, 504: { attempts: 0 }, 429: { attempts: 0 } },
    });
    const padded = JSON.stringify({ error: { message: "Please retry later" } }) + " ".repeat(200 * 1024);
    fetchMock
      .mockResolvedValueOnce(new Response(padded, { status: 400 }))
      .mockResolvedValueOnce(new Response("{}", { status: 200 }));
    const ex = new BaseExecutor("cap-prov", { baseUrl: "https://x/api", retry: { 502: { attempts: 0 }, 503: { attempts: 0 }, 504: { attempts: 0 }, 429: { attempts: 0 } } });
    const out = await ex.execute({ model: "m", body: {}, stream: false, credentials: { apiKey: "k" } });
    clearRetryPlugins();
    expect(out.response.status).toBe(200);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});
