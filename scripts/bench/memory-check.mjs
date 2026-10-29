#!/usr/bin/env node
// Peak-memory check of the Node build under two Cloudflare-relevant stress shapes:
//
//   big-body   a chat request whose JSON body is ~50 MB (a base64 image inside the messages)
//   long-sse   a streamed chat response of ~200 MB of SSE from the upstream
//
//   node scripts/bench/memory-check.mjs [--scenarios big-body,long-sse] [--node-args --max-old-space-size=128]
//        [--env NINEROUTER_MAX_BODY_MB=20,NINEROUTER_STREAM_CAPTURE_MAX_BYTES=65536] [--label name]
//        [--body-mb 50] [--sse-mb 200]
//
// Every scenario runs against a fresh server process (custom-server.js) on a credential-free temp
// database with a local streaming mock upstream, so the peak is attributable to that one request.
// The peak is the kernel's VmHWM for the server process (peak resident set), plus whether the server
// still answered /api/health afterwards. Run it from two checkouts (or `git stash`) to compare code
// versions; --env sets server env vars, e.g. the Cloudflare limits, without needing a Worker.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
const argv = process.argv.slice(2);
const arg = (name, fallback) => { const i = argv.indexOf(`--${name}`); return i >= 0 ? argv[i + 1] : fallback; };
const list = (v) => String(v || "").split(",").map((s) => s.trim()).filter(Boolean);
const scenarios = list(arg("scenarios", "big-body,long-sse"));
const bodyMb = Number(arg("body-mb", 50));
const sseMb = Number(arg("sse-mb", 200));
const label = arg("label", "run");
const nodeArgs = arg("node-args", "") ? arg("node-args", "").split(" ").filter(Boolean) : [];
const extraEnv = Object.fromEntries(list(arg("env", "")).map((kv) => { const i = kv.indexOf("="); return [kv.slice(0, i), kv.slice(i + 1)]; }));
const port = Number(arg("port", 20131));
const mockPort = Number(arg("mock-port", 20198));

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "9r-mem-"));
const dataDir = path.join(tmp, "data");
const homeDir = path.join(tmp, "home");
fs.mkdirSync(homeDir, { recursive: true });
const children = [];
process.on("exit", () => { for (const c of children) { try { c.kill("SIGKILL"); } catch { /* gone */ } } fs.rmSync(tmp, { recursive: true, force: true }); });

const setup = spawnSync("node", ["scripts/bench/setup-bench-db.mjs", "--out", dataDir, "--mock-url", `http://127.0.0.1:${mockPort}/v1`], { cwd: root, encoding: "utf8" });
if (setup.status !== 0) { console.error(setup.stderr || setup.stdout); process.exit(1); }
const key = JSON.parse(setup.stdout).keys.plain;

// ---- streaming mock upstream ---------------------------------------------------------------------
const CHUNK_TEXT = "x".repeat(4000);
const mock = http.createServer(async (req, res) => {
  const chunks = [];
  for await (const c of req) chunks.push(c);       // the mock itself may hold the request; it is not what is measured
  let body = {};
  try { body = JSON.parse(Buffer.concat(chunks).toString("utf8")); } catch { /* ignore */ }
  if (!body.stream) {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ id: "m", object: "chat.completion", created: 0, model: "mock-model", choices: [{ index: 0, message: { role: "assistant", content: "ok" }, finish_reason: "stop" }], usage: { prompt_tokens: 5, completion_tokens: 1, total_tokens: 6 } }));
    return;
  }
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
  const frame = `data: ${JSON.stringify({ id: "m", object: "chat.completion.chunk", created: 0, model: "mock-model", choices: [{ index: 0, delta: { content: CHUNK_TEXT }, finish_reason: null }] })}\n\n`;
  const frames = Math.ceil((sseMb * 1024 * 1024) / frame.length);
  for (let i = 0; i < frames; i++) {
    if (!res.write(frame)) await new Promise((r) => res.once("drain", r));
    if (res.destroyed) return;
  }
  res.write(`data: ${JSON.stringify({ id: "m", object: "chat.completion.chunk", created: 0, model: "mock-model", choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 5, completion_tokens: 50000, total_tokens: 50005 } })}\n\ndata: [DONE]\n\n`);
  res.end();
});
await new Promise((r) => mock.listen(mockPort, "127.0.0.1", r));
children.push({ kill: () => mock.close() });

// ---- server ---------------------------------------------------------------------------------------
const serverEnv = {
  ...process.env, HOME: homeDir, DATA_DIR: dataDir, PORT: String(port), HOSTNAME: "127.0.0.1", NODE_ENV: "production",
  HTTP_PROXY: "http://127.0.0.1:9", HTTPS_PROXY: "http://127.0.0.1:9", NO_PROXY: "127.0.0.1,localhost", LOG_LEVEL: "ERROR",
  ...extraEnv,
};
delete serverEnv.NINEROUTER_RUNTIME;
const origin = `http://127.0.0.1:${port}`;

async function startServer() {
  const log = fs.openSync(path.join(tmp, "server.log"), "a");
  const server = spawn("node", ["--disable-warning=MODULE_TYPELESS_PACKAGE_JSON", ...nodeArgs, "custom-server.js", "--port", String(port)], { cwd: root, env: serverEnv, stdio: ["ignore", log, log] });
  children.push(server);
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    if (server.exitCode !== null) throw new Error("server exited during boot");
    try { if ((await fetch(`${origin}/api/health`)).ok) return server; } catch { /* not up */ }
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error("server did not become healthy");
}
const peakRssMb = (pid) => {
  try { return Number(/VmHWM:\s+(\d+)/.exec(fs.readFileSync(`/proc/${pid}/status`, "utf8"))[1]) / 1024; } catch { return null; }
};
const rssMb = (pid) => {
  try { return Number(/VmRSS:\s+(\d+)/.exec(fs.readFileSync(`/proc/${pid}/status`, "utf8"))[1]) / 1024; } catch { return null; }
};
const alive = async () => { try { return (await fetch(`${origin}/api/health`, { signal: AbortSignal.timeout(5000) })).ok; } catch { return false; } };

const chatHeaders = { "content-type": "application/json", authorization: `Bearer ${key}` };
const smallChat = (stream) => JSON.stringify({ model: "mock/mock-model", stream, messages: [{ role: "user", content: "hello" }] });

async function runScenario(name) {
  const server = await startServer();
  // Warm the request path so lazy module loading is not counted as the scenario's peak.
  await (await fetch(`${origin}/v1/chat/completions`, { method: "POST", headers: chatHeaders, body: smallChat(false) })).text();
  const baseRss = rssMb(server.pid);
  const t0 = Date.now();
  let status = null, error = null, received = 0;
  try {
    if (name === "big-body") {
      const b64 = Buffer.alloc(Math.floor((bodyMb * 1024 * 1024 * 3) / 4), 7).toString("base64");
      const body = JSON.stringify({ model: "mock/mock-model", stream: false, messages: [{ role: "user", content: [{ type: "text", text: "describe" }, { type: "image_url", image_url: { url: `data:image/png;base64,${b64}` } }] }] });
      received = body.length;
      const res = await fetch(`${origin}/v1/chat/completions`, { method: "POST", headers: chatHeaders, body, signal: AbortSignal.timeout(300_000) });
      status = res.status;
      await res.arrayBuffer();
    } else {
      const res = await fetch(`${origin}/v1/chat/completions`, { method: "POST", headers: chatHeaders, body: smallChat(true), signal: AbortSignal.timeout(600_000) });
      status = res.status;
      const reader = res.body.getReader();
      for (;;) { const { done, value } = await reader.read(); if (done) break; received += value.byteLength; }
    }
  } catch (e) {
    error = String(e?.cause?.code || e?.message || e);
  }
  const survived = await alive();
  const result = {
    label, scenario: name, status, error, survived, seconds: +((Date.now() - t0) / 1000).toFixed(1),
    clientBytes: received, rssMbBefore: +baseRss.toFixed(0), rssMbPeak: peakRssMb(server.pid) == null ? null : +peakRssMb(server.pid).toFixed(0),
    nodeArgs: nodeArgs.join(" "), env: extraEnv,
  };
  server.kill("SIGKILL");
  await new Promise((r) => setTimeout(r, 800));
  return result;
}

const results = [];
for (const name of scenarios) {
  results.push(await runScenario(name));
  console.log(JSON.stringify(results.at(-1)));
}
process.exit(0);
