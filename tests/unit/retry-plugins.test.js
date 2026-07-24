import { describe, it, expect, beforeEach, afterEach } from "vitest";
import {
  registerRetryPlugin,
  clearRetryPlugins,
  resolveRetryPlugin,
  resolveHttpRetryConfig,
  resolveFallbackDecision,
  resolveHttpTextRetry,
  normalizeUserRetryConfig,
  matchErrorRule,
  normalizeErrorRule,
  normalizeErrorText,
  errorTextIncludes,
  findMatchingErrorText,
} from "../../open-sse/retry/index.js";
import { checkFallbackError as checkFallbackErrorFromService } from "../../open-sse/services/accountFallback.js";
import { DEFAULT_RETRY_CONFIG } from "../../open-sse/config/runtimeConfig.js";
import { TRANSIENT_COOLDOWN_MS } from "../../open-sse/config/errorConfig.js";

beforeEach(() => clearRetryPlugins());
afterEach(() => clearRetryPlugins());

describe("normalizeErrorText + errorTextIncludes (central lower.includes)", () => {
  it("lowercases strings", () => {
    expect(normalizeErrorText("Insufficient Tokens")).toBe("insufficient tokens");
  });

  it("stringifies objects then lowercases", () => {
    expect(normalizeErrorText({ error: { message: "Rate Limit" } })).toContain("rate limit");
  });

  it("errorTextIncludes is case-insensitive substring match", () => {
    expect(errorTextIncludes("ERROR: Insufficient Tokens left", "insufficient tokens")).toBe(true);
    expect(errorTextIncludes("ERROR: Insufficient Tokens left", "INSUFFICIENT TOKENS")).toBe(true);
    expect(errorTextIncludes("ok", "boom")).toBe(false);
    expect(errorTextIncludes(null, "x")).toBe(false);
    expect(errorTextIncludes("hello", "")).toBe(false);
  });

  it("findMatchingErrorText returns first hit by order", () => {
    const hit = findMatchingErrorText(
      "You hit the free daily limit",
      ["quota exceeded", "free daily limit", "rate limit"]
    );
    expect(hit).toBe("free daily limit");
  });

  it("matchErrorRule uses lower.includes for text before status", () => {
    const rules = [
      normalizeErrorRule({ text: "Please Retry", retryHttp: true, attempts: 2 }),
      normalizeErrorRule({ status: 500, cooldownMs: 1000 }),
    ];
    const m = matchErrorRule(rules, 500, "upstream said: please retry later");
    expect(m.text).toBe("please retry");
    expect(m.retryHttp).toBe(true);
  });
});

describe("checkFallbackError always uses central matcher", () => {
  it("accountFallback.checkFallbackError matches global text rules via lower.includes", () => {
    const d = checkFallbackErrorFromService(400, "Request Not Allowed by policy", 0);
    expect(d.shouldFallback).toBe(true);
    expect(d.cooldownMs).toBe(5000); // COOLDOWN.short for "request not allowed"
  });

  it("accountFallback.checkFallbackError matches rate limit text with backoff", () => {
    const d = checkFallbackErrorFromService(200, "Rate Limit exceeded for key", 0);
    expect(d.shouldFallback).toBe(true);
    expect(d.newBackoffLevel).toBe(1);
  });
});

describe("normalizeUserRetryConfig", () => {
  it("normalizes text rules and httpRetry map", () => {
    const cfg = normalizeUserRetryConfig({
      errorRules: [{ text: "Insufficient Tokens", cooldownMs: 5000, retryHttp: true, attempts: 2 }],
      httpRetry: { 502: { attempts: 5, delayMs: 100 } },
    });
    expect(cfg.errorRules[0].text).toBe("insufficient tokens");
    expect(cfg.errorRules[0].retryHttp).toBe(true);
    expect(cfg.errorRules[0].attempts).toBe(2);
    expect(cfg.httpRetry[502]).toEqual({ attempts: 5, delayMs: 100 });
  });

  it("returns null for empty config", () => {
    expect(normalizeUserRetryConfig({})).toBeNull();
    expect(normalizeUserRetryConfig(null)).toBeNull();
  });
});

describe("matchErrorRule", () => {
  it("matches text before status", () => {
    const rules = [
      normalizeErrorRule({ text: "quota", backoff: true }),
      normalizeErrorRule({ status: 429, cooldownMs: 1000 }),
    ];
    const m = matchErrorRule(rules, 429, "Quota exceeded for project");
    expect(m.text).toBe("quota");
    expect(m.backoff).toBe(true);
  });
});

describe("registerRetryPlugin + resolve", () => {
  it("merges registered plugin httpRetry over defaults", () => {
    registerRetryPlugin("demo-prov", {
      httpRetry: { 502: { attempts: 9, delayMs: 10 } },
      errorRules: [{ text: "no seats", cooldownMs: 12000 }],
    });
    const http = resolveHttpRetryConfig("demo-prov");
    expect(http[502].attempts).toBe(9);
    expect(http[503].attempts).toBe(DEFAULT_RETRY_CONFIG[503].attempts);
  });

  it("node credentials.retry overrides registry/plugin http", () => {
    registerRetryPlugin("demo-prov", {
      httpRetry: { 502: { attempts: 1, delayMs: 10 } },
    });
    const http = resolveHttpRetryConfig("demo-prov", {
      providerSpecificData: {
        retry: { httpRetry: { 502: { attempts: 7, delayMs: 1 } } },
      },
    });
    expect(http[502].attempts).toBe(7);
  });

  it("settings override wins over node retry", () => {
    const plugin = resolveRetryPlugin(
      "demo-prov",
      { providerSpecificData: { retry: { httpRetry: { 502: { attempts: 2 } } } } },
      { httpRetry: { 502: { attempts: 11, delayMs: 0 } } }
    );
    expect(plugin.httpRetry[502].attempts).toBe(11);
  });

  it("prefix match applies to openai-compatible-*", () => {
    registerRetryPlugin({
      id: "openai-compatible-",
      matchMode: "prefix",
      errorRules: [{ text: "rate limited by gateway", backoff: true }],
    });
    const d = resolveFallbackDecision(
      400,
      "Rate limited by gateway",
      0,
      "openai-compatible-chat-abc"
    );
    expect(d.shouldFallback).toBe(true);
    expect(d.newBackoffLevel).toBe(1);
  });
});

describe("resolveFallbackDecision", () => {
  it("uses global ERROR_RULES when no plugin", () => {
    const d = resolveFallbackDecision(429, "too many requests", 0, "unknown-x");
    expect(d.shouldFallback).toBe(true);
    expect(d.newBackoffLevel).toBe(1);
  });

  it("plugin shouldFallback:false blocks account fallback", () => {
    registerRetryPlugin("no-fb", {
      errorRules: [{ text: "validation failed", shouldFallback: false }],
    });
    const d = resolveFallbackDecision(400, "validation failed", 0, "no-fb");
    expect(d.shouldFallback).toBe(false);
    expect(d.cooldownMs).toBe(0);
  });

  it("unmatched falls back to transient cooldown", () => {
    const d = resolveFallbackDecision(418, "teapot", 0, "nobody");
    expect(d.shouldFallback).toBe(true);
    expect(d.cooldownMs).toBe(TRANSIENT_COOLDOWN_MS);
  });
});

describe("resolveHttpTextRetry", () => {
  it("returns attempts/delay when rule has retryHttp", () => {
    registerRetryPlugin("txt", {
      errorRules: [
        { text: "temporary glitch", retryHttp: true, attempts: 3, delayMs: 50 },
      ],
    });
    const r = resolveHttpTextRetry("txt", 500, "Temporary Glitch — try again");
    expect(r).toMatchObject({ attempts: 3, delayMs: 50 });
  });

  it("returns null when no retryHttp rule matches", () => {
    registerRetryPlugin("txt2", {
      errorRules: [{ text: "temporary glitch", cooldownMs: 1000 }],
    });
    expect(resolveHttpTextRetry("txt2", 500, "Temporary Glitch")).toBeNull();
  });
});
