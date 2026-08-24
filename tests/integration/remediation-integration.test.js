import { afterAll, beforeAll, describe, expect, it } from "vitest";
import http from "node:http";
import { BaseExecutor } from "../../open-sse/executors/base.js";
import { DefaultExecutor } from "../../open-sse/executors/default.js";

const received = new Map();
let server;
let baseUrl;

function readJson(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      try {
        resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")));
      } catch (error) {
        reject(error);
      }
    });
    req.on("error", reject);
  });
}

function sendJson(res, status, body) {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(body));
}

beforeAll(async () => {
  server = http.createServer(async (req, res) => {
    const requests = received.get(req.url) || [];
    const body = await readJson(req);
    requests.push(body);
    received.set(req.url, requests);

    if (req.url === "/responses") {
      if (body.max_completion_tokens !== undefined) {
        sendJson(res, 400, {
          error: {
            message: "Unsupported parameter: 'max_completion_tokens'. In the Responses API, this parameter has moved to 'max_output_tokens'. Try again with the new parameter.",
            type: "invalid_request_error",
            code: "unsupported_parameter",
          },
        });
        return;
      }
      sendJson(res, 200, { ok: true });
      return;
    }

    if (req.url === "/responses-reasoning") {
      if (body.reasoning_effort !== undefined) {
        sendJson(res, 400, {
          error: {
            message: "Unsupported parameter: 'reasoning_effort'. In the Responses API, this parameter has moved to 'reasoning.effort'. Try again with the new parameter.",
            type: "invalid_request_error",
            code: "unsupported_parameter",
          },
        });
        return;
      }
      sendJson(res, 200, { ok: true });
      return;
    }

    if (req.url === "/canonical/responses") {
      if (body.reasoning_effort !== undefined || body.max_completion_tokens !== undefined) {
        sendJson(res, 400, { error: { message: "legacy Responses parameters reached the wire" } });
        return;
      }
      sendJson(res, 200, { ok: true });
      return;
    }

    if (req.url === "/messages") {
      if (body.thinking?.type === "enabled") {
        sendJson(res, 400, {
          type: "error",
          error: {
            type: "invalid_request_error",
            message: '"thinking.type.enabled" is not supported for this model. Use "thinking.type.adaptive" and "output_config.effort" to control thinking behavior.',
          },
        });
        return;
      }
      sendJson(res, 200, { ok: true });
      return;
    }

    sendJson(res, 404, { error: "unknown demo endpoint" });
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  baseUrl = `http://127.0.0.1:${server.address().port}`;
});

afterAll(async () => {
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
});

describe("request remediation against an error demo server", () => {
  it("renames a rejected Responses API parameter and retries the corrected request", async () => {
    received.delete("/responses");
    const executor = new BaseExecutor("demo-responses", {
      baseUrl: `${baseUrl}/responses`,
      retry: {},
    });

    const result = await executor.execute({
      model: "demo-model",
      body: { input: "hello", max_completion_tokens: 321 },
      stream: false,
      credentials: { apiKey: "demo" },
    });

    expect(result.response.status).toBe(200);
    expect(received.get("/responses")).toEqual([
      { input: "hello", max_completion_tokens: 321 },
      { input: "hello", max_output_tokens: 321 },
    ]);
  });

  it("replaces rejected Claude thinking arguments and retries the corrected request", async () => {
    received.delete("/messages");
    const executor = new BaseExecutor("demo-claude", {
      baseUrl: `${baseUrl}/messages`,
      retry: {},
    });

    const result = await executor.execute({
      model: "demo-model",
      body: {
        messages: [{ role: "user", content: "hello" }],
        thinking: { type: "enabled", budget_tokens: 25000 },
      },
      stream: false,
      credentials: { apiKey: "demo" },
    });

    expect(result.response.status).toBe(200);
    expect(received.get("/messages")).toEqual([
      {
        messages: [{ role: "user", content: "hello" }],
        thinking: { type: "enabled", budget_tokens: 25000 },
      },
      {
        messages: [{ role: "user", content: "hello" }],
        thinking: { type: "adaptive" },
        output_config: { effort: "high" },
      },
    ]);
  });

  it("moves a rejected flat argument into the reported nested path", async () => {
    received.delete("/responses-reasoning");
    const executor = new BaseExecutor("demo-responses", {
      baseUrl: `${baseUrl}/responses-reasoning`,
      retry: {},
    });

    const result = await executor.execute({
      model: "demo-model",
      body: { input: "hello", reasoning_effort: "high" },
      stream: false,
      credentials: { apiKey: "demo" },
    });

    expect(result.response.status).toBe(200);
    expect(received.get("/responses-reasoning")).toEqual([
      { input: "hello", reasoning_effort: "high" },
      { input: "hello", reasoning: { effort: "high" } },
    ]);
  });

  it("canonicalizes Responses parameters before the first network request", async () => {
    received.delete("/canonical/responses");
    const executor = new DefaultExecutor("openai-compatible-responses-demo");

    const result = await executor.execute({
      model: "gpt-5.4",
      body: { input: "hello", max_completion_tokens: 123, reasoning_effort: "high" },
      stream: false,
      credentials: {
        apiKey: "demo",
        providerSpecificData: {
          apiType: "responses",
          baseUrl: `${baseUrl}/canonical/responses`,
        },
      },
    });

    expect(result.response.status).toBe(200);
    expect(received.get("/canonical/responses")).toEqual([
      { input: "hello", max_output_tokens: 123, reasoning: { effort: "high" } },
    ]);
  });
});
