#!/usr/bin/env node
// Tiny OpenAI-compatible mock upstream for load tests and D1-op accounting.
// No real provider is contacted and no credentials are spent, so measurements
// isolate the router's own overhead.
//
//   node scripts/bench/mock-upstream.mjs            # listens on MOCK_PORT (default 20199)
//
// Behaviour (per request):
//   POST /v1/chat/completions   stream:true -> SSE (MOCK_SSE_CHUNKS chunks + usage + [DONE])
//                               stream:false -> one JSON completion with usage
//   header x-mock-fail: <status>, or model "fail-<status>" -> answer that HTTP status with an error body
//   MOCK_DELAY_MS               -> fixed latency before the first byte (default 0)
//   GET  /v1/models             -> small model list
import http from "node:http";

const COMPLETION_TEXT = "Hello from the mock upstream.";

export function startMockUpstream({ port = 0, delayMs = 0, sseChunks = 8, host = "127.0.0.1" } = {}) {
  const stats = { requests: 0, streams: 0, failures: 0 };
  const sleep = (ms) => (ms > 0 ? new Promise((r) => setTimeout(r, ms)) : Promise.resolve());

  const server = http.createServer(async (req, res) => {
    stats.requests++;
    const url = new URL(req.url, "http://mock");
    if (req.method === "GET" && url.pathname === "/v1/models") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ object: "list", data: [{ id: "mock-model", object: "model", created: 0, owned_by: "mock" }] }));
      return;
    }
    if (req.method !== "POST" || !url.pathname.endsWith("/chat/completions")) {
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: "not found" } }));
      return;
    }

    const chunks = [];
    for await (const c of req) chunks.push(c);
    let body = {};
    try { body = JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}"); } catch { /* keep {} */ }

    await sleep(delayMs);
    // Failure injection: header x-mock-fail, or a model id like "fail-500".
    const modelFail = /^fail-(\d{3})$/.exec(String(body.model || ""));
    const failStatus = Number(req.headers["x-mock-fail"] || (modelFail ? modelFail[1] : 0));
    if (Number.isInteger(failStatus) && failStatus >= 400) {
      stats.failures++;
      res.writeHead(failStatus, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: "mock upstream failure", type: "mock_error" } }));
      return;
    }

    const usage = { prompt_tokens: 12, completion_tokens: 8, total_tokens: 20 };
    const id = `chatcmpl-mock-${stats.requests}`;
    const model = body.model || "mock-model";
    if (body.stream) {
      stats.streams++;
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
      const words = COMPLETION_TEXT.split(" ");
      for (let i = 0; i < sseChunks; i++) {
        const delta = i === 0 ? { role: "assistant", content: words[0] } : { content: ` ${words[i % words.length]}` };
        res.write(`data: ${JSON.stringify({ id, object: "chat.completion.chunk", created: 0, model, choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`);
      }
      res.write(`data: ${JSON.stringify({ id, object: "chat.completion.chunk", created: 0, model, choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage })}\n\n`);
      res.end("data: [DONE]\n\n");
      return;
    }
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({
      id, object: "chat.completion", created: 0, model,
      choices: [{ index: 0, message: { role: "assistant", content: COMPLETION_TEXT }, finish_reason: "stop" }],
      usage,
    }));
  });
  server.keepAliveTimeout = 65_000;

  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, () => {
      const address = server.address();
      resolve({
        server,
        stats,
        port: address.port,
        url: `http://${host}:${address.port}`,
        close: () => new Promise((r) => { server.closeAllConnections?.(); server.close(() => r()); }),
      });
    });
  });
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const port = Number(process.env.MOCK_PORT || 20199);
  const delayMs = Number(process.env.MOCK_DELAY_MS || 0);
  const sseChunks = Number(process.env.MOCK_SSE_CHUNKS || 8);
  const mock = await startMockUpstream({ port, delayMs, sseChunks });
  console.log(`[mock-upstream] listening on ${mock.url} (delay=${delayMs}ms, sseChunks=${sseChunks})`);
}
