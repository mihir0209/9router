import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

const fetchMock = vi.fn();
vi.mock("../../open-sse/utils/proxyFetch.js", () => ({
  proxyAwareFetch: (...args) => fetchMock(...args),
}));

const { BaseExecutor } = await import("../../open-sse/executors/base.js");
const { registerRetryPlugin, clearRetryPlugins } = await import("../../open-sse/retry/index.js");

function res(status, body = "") {
  return {
    status,
    headers: { get: () => "application/json" },
    clone() {
      return {
        text: async () => body,
        json: async () => {
          try { return JSON.parse(body); } catch { return {}; }
        },
      };
    },
  };
}

beforeEach(() => {
  fetchMock.mockReset();
  clearRetryPlugins();
});
afterEach(() => clearRetryPlugins());

describe("BaseExecutor — plugin text HTTP retry", () => {
  it("retries same url when error body matches retryHttp text rule", async () => {
    registerRetryPlugin("test-prov", {
      errorRules: [
        { text: "please retry", retryHttp: true, attempts: 2, delayMs: 0 },
      ],
      httpRetry: { 502: { attempts: 0 }, 503: { attempts: 0 }, 504: { attempts: 0 }, 429: { attempts: 0 } },
    });

    const ex = new BaseExecutor("test-prov", {
      baseUrl: "https://x/api",
      retry: { 502: { attempts: 0 }, 503: { attempts: 0 }, 504: { attempts: 0 }, 429: { attempts: 0 } },
    });

    fetchMock
      .mockResolvedValueOnce(res(400, JSON.stringify({ error: { message: "Please retry later" } })))
      .mockResolvedValueOnce(res(200, "{}"));

    const out = await ex.execute({
      model: "m",
      body: {},
      stream: false,
      credentials: { apiKey: "k" },
    });
    expect(out.response.status).toBe(200);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("uses credentials.providerSpecificData.retry without register", async () => {
    const ex = new BaseExecutor("custom-node", {
      baseUrl: "https://x/api",
      retry: { 502: { attempts: 0 }, 503: { attempts: 0 }, 504: { attempts: 0 }, 429: { attempts: 0 } },
    });

    fetchMock
      .mockResolvedValueOnce(res(503, "upstream overloaded briefly"))
      .mockResolvedValueOnce(res(200, "{}"));

    const out = await ex.execute({
      model: "m",
      body: {},
      stream: false,
      credentials: {
        apiKey: "k",
        providerSpecificData: {
          retry: {
            errorRules: [{ text: "overloaded briefly", retryHttp: true, attempts: 1, delayMs: 0 }],
            httpRetry: { 503: { attempts: 0 } },
          },
        },
      },
    });
    expect(out.response.status).toBe(200);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("rewrites exhausted retryable text errors to 429", async () => {
    registerRetryPlugin("testprov", {
      errorRules: [
        { text: "upstream service temporarily unavailable", retryHttp: true, attempts: 1, delayMs: 0 },
      ],
    });

    fetchMock.mockResolvedValue(res(503, "Upstream service temporarily unavailable"));

    const executor = new BaseExecutor("testprov", { baseUrl: "http://localhost:9999/v1" });
    const out = await executor.execute({
      model: "some-model",
      body: { messages: [] },
      stream: false,
      credentials: { apiKey: "k" },
      signal: null,
      log: { info: vi.fn(), warn: vi.fn(), error: vi.fn() },
    });

    expect(out.response.status).toBe(429);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});
