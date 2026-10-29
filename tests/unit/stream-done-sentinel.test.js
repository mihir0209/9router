// A Chat Completions client always receives exactly one `data: [DONE]`, whatever the upstream sends:
// OpenAI-style SSE with or without its own sentinel, Ollama NDJSON, Claude SSE. Clients in other formats
// (Claude, Responses, Gemini) must not get one added.
import { describe, expect, it } from "vitest";
import { FORMATS } from "../../open-sse/translator/formats.js";
import { createSSETransformStreamWithLogger, createPassthroughStreamWithLogger } from "../../open-sse/utils/stream.js";

async function run(ts, parts) {
  const enc = new TextEncoder();
  const src = new ReadableStream({ start(c) { for (const p of parts) c.enqueue(enc.encode(p)); c.close(); } });
  const reader = src.pipeThrough(ts).getReader();
  const dec = new TextDecoder();
  let out = "";
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    out += dec.decode(value, { stream: true });
  }
  return out;
}
const dones = (t) => (t.match(/data:\s*\[DONE\]/g) || []).length;
const endsWithDone = (t) => /data: \[DONE\]\n\n$/.test(t);

const nd = (content, done = false) => JSON.stringify({
  model: "m", message: { role: "assistant", content }, done,
  ...(done ? { done_reason: "stop", prompt_eval_count: 1, eval_count: 2 } : {}),
}) + "\n";
const oa = (content, finish) => "data: " + JSON.stringify({
  id: "x", object: "chat.completion.chunk", model: "m",
  choices: [{ index: 0, delta: content ? { content } : {}, finish_reason: finish || null }],
  ...(finish ? { usage: { prompt_tokens: 1, completion_tokens: 2, total_tokens: 3 } } : {}),
}) + "\n\n";
const claude = [
  'event: message_start\ndata: {"type":"message_start","message":{"id":"1","model":"m","usage":{"input_tokens":1}}}\n\n',
  'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n',
  'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}\n\n',
  'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}\n\n',
  'event: message_stop\ndata: {"type":"message_stop"}\n\n',
];

describe("OpenAI-format clients: exactly one [DONE]", () => {
  it("Ollama NDJSON upstream (no sentinel of its own)", async () => {
    const out = await run(createSSETransformStreamWithLogger(FORMATS.OLLAMA, FORMATS.OPENAI, "ollama", null, null, "m"), [nd("a"), nd("", true)]);
    expect(dones(out)).toBe(1);
    expect(endsWithDone(out)).toBe(true);
    expect(out).toContain('"finish_reason":"stop"');
  });

  it("Ollama NDJSON whose last line has no newline", async () => {
    const out = await run(createSSETransformStreamWithLogger(FORMATS.OLLAMA, FORMATS.OPENAI, "ollama", null, null, "m"), [nd("a"), nd("", true).trimEnd()]);
    expect(dones(out)).toBe(1);
    expect(endsWithDone(out)).toBe(true);
  });

  it("Claude SSE upstream", async () => {
    const out = await run(createSSETransformStreamWithLogger(FORMATS.CLAUDE, FORMATS.OPENAI, "claude", null, null, "m"), claude);
    expect(dones(out)).toBe(1);
    expect(endsWithDone(out)).toBe(true);
  });

  it("OpenAI SSE translated (upstream without sentinel)", async () => {
    const out = await run(createSSETransformStreamWithLogger(FORMATS.OPENAI, FORMATS.OPENAI, "openai", null, null, "m"), [oa("a"), oa("", "stop")]);
    expect(dones(out)).toBe(1);
  });

  it("OpenAI SSE translated (upstream sends its own sentinel): not duplicated", async () => {
    const out = await run(createSSETransformStreamWithLogger(FORMATS.OPENAI, FORMATS.OPENAI, "openai", null, null, "m"), [oa("a"), oa("", "stop"), "data: [DONE]\n\n"]);
    expect(dones(out)).toBe(1);
    expect(endsWithDone(out)).toBe(true);
  });

  it("passthrough without upstream sentinel", async () => {
    const out = await run(createPassthroughStreamWithLogger("openai", null, "m"), [oa("a"), oa("", "stop")]);
    expect(dones(out)).toBe(1);
    expect(endsWithDone(out)).toBe(true);
  });

  it("passthrough with upstream sentinel (with and without trailing newline, split across chunks)", async () => {
    for (const tail of [["data: [DONE]\n\n"], ["data: [DONE]"], ["data: [DO", "NE]\n\n"], ["data: [DONE]\n"]]) {
      const out = await run(createPassthroughStreamWithLogger("openai", null, "m"), [oa("a"), oa("", "stop"), ...tail]);
      expect(dones(out), JSON.stringify(tail)).toBe(1);
    }
  });

  it("passthrough: an empty upstream body still terminates", async () => {
    const out = await run(createPassthroughStreamWithLogger("openai", null, "m"), []);
    expect(dones(out)).toBe(1);
  });
});

describe("other client formats are left alone", () => {
  it("a Claude client does not receive [DONE]", async () => {
    const out = await run(createSSETransformStreamWithLogger(FORMATS.CLAUDE, FORMATS.CLAUDE, "claude", null, null, "m"), claude);
    expect(dones(out)).toBe(0);
  });

  it("a Gemini-family passthrough does not receive [DONE]", async () => {
    const out = await run(createPassthroughStreamWithLogger("gemini", null, "m"), ['data: {"candidates":[]}\n\n']);
    expect(dones(out)).toBe(0);
  });

  it("an Ollama upstream for a Claude client gets no [DONE]", async () => {
    const out = await run(createSSETransformStreamWithLogger(FORMATS.OLLAMA, FORMATS.CLAUDE, "ollama", null, null, "m"), [nd("a"), nd("", true)]);
    expect(dones(out)).toBe(0);
  });
});
