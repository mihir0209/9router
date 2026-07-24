import { describe, it, expect } from "vitest";
import { matchHttpTextRetry, normalizeRetryPlugin } from "../../open-sse/retry/schema.js";

describe("Global retry rules in matchHttpTextRetry", () => {
  it("should match 'temporarily unavailable' from global rules", () => {
    const plugin = normalizeRetryPlugin({
      id: "test",
      errorRules: [],
    });
    
    const result = matchHttpTextRetry(plugin, 503, "upstream service temporarily unavailable");
    expect(result).not.toBeNull();
    expect(result.rule.text).toBe("temporarily unavailable");
    expect(result.rule.retryHttp).toBe(true);
  });

  it("should match 'service unavailable' from global rules", () => {
    const plugin = normalizeRetryPlugin({
      id: "test",
      errorRules: [],
    });
    
    const result = matchHttpTextRetry(plugin, 503, "service unavailable");
    expect(result).not.toBeNull();
    expect(result.rule.text).toBe("service unavailable");
    expect(result.rule.retryHttp).toBe(true);
  });

  it("plugin rules should take priority over global rules", () => {
    const plugin = normalizeRetryPlugin({
      id: "test",
      errorRules: [
        { text: "custom error", retryHttp: true, attempts: 5, delayMs: 100 },
      ],
    });
    
    // Plugin rule should match first
    const result = matchHttpTextRetry(plugin, 500, "custom error in response");
    expect(result).not.toBeNull();
    expect(result.rule.text).toBe("custom error");
    expect(result.attempts).toBe(5);
    expect(result.delayMs).toBe(100);
  });

  it("should fall back to global rules when plugin rules don't match", () => {
    const plugin = normalizeRetryPlugin({
      id: "test",
      errorRules: [
        { text: "custom error", retryHttp: true },
      ],
    });
    
    // Plugin rule doesn't match, global rule should match
    const result = matchHttpTextRetry(plugin, 503, "service temporarily unavailable");
    expect(result).not.toBeNull();
    expect(result.rule.text).toBe("temporarily unavailable");
  });

  it("should return null when no rules match", () => {
    const plugin = normalizeRetryPlugin({
      id: "test",
      errorRules: [],
    });
    
    const result = matchHttpTextRetry(plugin, 200, "success");
    expect(result).toBeNull();
  });
});
