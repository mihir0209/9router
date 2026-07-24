import { describe, it, expect, beforeAll, afterAll } from "vitest";
import http from "http";

// Test server that returns errors then success
let testServer;
let requestCount = 0;
const PORT = 29998;

function startTestServer(errorCount = 3) {
  requestCount = 0;
  return new Promise((resolve) => {
    testServer = http.createServer((req, res) => {
      requestCount++;
      const path = new URL(req.url, `http://localhost:${PORT}`).pathname;

      if (path === "/health") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ ok: true }));
        return;
      }

      if (path === "/reset") {
        requestCount = 0;
        res.writeHead(200, { "Content-Type": "text/plain" });
        res.end("reset");
        return;
      }

      if (requestCount <= errorCount) {
        res.writeHead(429, { "Content-Type": "application/json" });
        res.end(JSON.stringify({
          error: {
            message: `rate limit exceeded (attempt ${requestCount}/${errorCount})`,
            type: "rate_limit_error",
            code: "rate_limit_exceeded"
          }
        }));
        return;
      }

      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({
        id: `chatcmpl-test-${Date.now()}`,
        object: "chat.completion",
        created: Math.floor(Date.now() / 1000),
        model: "test-model",
        choices: [{
          index: 0,
          message: {
            role: "assistant",
            content: `Success after ${requestCount - 1} retries!`
          },
          finish_reason: "stop"
        }],
        usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 }
      }));
    });

    testServer.listen(PORT, "127.0.0.1", () => {
      resolve(testServer);
    });
  });
}

describe("Retry Integration Tests", () => {
  beforeAll(async () => {
    await startTestServer(3);
  });

  afterAll(() => {
    if (testServer) {
      testServer.close();
    }
  });

  it("test server returns errors then success", async () => {
    // Reset counter
    await fetch(`http://127.0.0.1:${PORT}/reset`);

    // Make requests until success
    let response;
    let attempts = 0;
    do {
      attempts++;
      response = await fetch(`http://127.0.0.1:${PORT}/chat/completions`);
    } while (response.status !== 200 && attempts < 10);

    expect(response.status).toBe(200);
    expect(attempts).toBe(4); // 3 errors + 1 success

    const data = await response.json();
    expect(data.choices[0].message.content).toContain("Success after 3 retries");
  });

  it("verify error response format", async () => {
    await fetch(`http://127.0.0.1:${PORT}/reset`);

    const response = await fetch(`http://127.0.0.1:${PORT}/chat/completions`);
    expect(response.status).toBe(429);

    const data = await response.json();
    expect(data.error.code).toBe("rate_limit_exceeded");
    expect(data.error.message).toContain("rate limit exceeded");
  });
});
