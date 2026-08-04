import { describe, it, expect } from "vitest";
import { resolveHeaderValue, resolveHeaderTemplates, applyHeaderBlacklist } from "../../open-sse/utils/headerTemplates.js";

describe("headerTemplates", () => {
  describe("resolveHeaderValue", () => {
    it("should return static strings unchanged", () => {
      expect(resolveHeaderValue("static-value")).toBe("static-value");
      expect(resolveHeaderValue("WorkBuddy/5.2.7")).toBe("WorkBuddy/5.2.7");
    });

    it("should resolve {uuid} to a valid UUID", () => {
      const result = resolveHeaderValue("{uuid}");
      expect(result).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
    });

    it("should resolve {uuid-nodash} to a UUID without dashes", () => {
      const result = resolveHeaderValue("{uuid-nodash}");
      expect(result).toMatch(/^[0-9a-f]{32}$/);
    });

    it("should resolve {timestamp} to milliseconds", () => {
      const result = resolveHeaderValue("{timestamp}");
      expect(Number(result)).toBeGreaterThan(Date.now() - 1000);
      expect(Number(result)).toBeLessThan(Date.now() + 1000);
    });

    it("should resolve {timestamp-sec} to seconds", () => {
      const result = resolveHeaderValue("{timestamp-sec}");
      const nowSec = Math.floor(Date.now() / 1000);
      expect(Number(result)).toBeGreaterThanOrEqual(nowSec - 1);
      expect(Number(result)).toBeLessThanOrEqual(nowSec + 1);
    });

    it("should resolve {random-hex-8} to 8 hex characters", () => {
      const result = resolveHeaderValue("{random-hex-8}");
      expect(result).toMatch(/^[0-9a-f]{8}$/);
    });

    it("should resolve {random-hex-16} to 16 hex characters", () => {
      const result = resolveHeaderValue("{random-hex-16}");
      expect(result).toMatch(/^[0-9a-f]{16}$/);
    });

    it("should resolve {random-hex-32} to 32 hex characters", () => {
      const result = resolveHeaderValue("{random-hex-32}");
      expect(result).toMatch(/^[0-9a-f]{32}$/);
    });

    it("should resolve {iso8601} to ISO 8601 timestamp", () => {
      const result = resolveHeaderValue("{iso8601}");
      expect(result).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);
    });

    it("should resolve multiple templates in one string", () => {
      const result = resolveHeaderValue("00-{uuid-nodash}-{random-hex-16}-01");
      expect(result).toMatch(/^00-[0-9a-f]{32}-[0-9a-f]{16}-01$/);
    });

    it("should generate new values for each placeholder occurrence", () => {
      const result = resolveHeaderValue("{uuid}:{uuid}");
      const parts = result.split(":");
      expect(parts).toHaveLength(2);
      expect(parts[0]).not.toBe(parts[1]); // Different UUIDs
    });

    it("should handle non-string values", () => {
      expect(resolveHeaderValue(123)).toBe(123);
      expect(resolveHeaderValue(null)).toBe(null);
      expect(resolveHeaderValue(undefined)).toBe(undefined);
    });
  });

  describe("resolveHeaderTemplates", () => {
    it("should resolve all header values in an object", () => {
      const headers = {
        "X-Request-ID": "{uuid}",
        "X-Static": "value",
        "X-Timestamp": "{timestamp}",
      };
      const resolved = resolveHeaderTemplates(headers);
      expect(resolved["X-Static"]).toBe("value");
      expect(resolved["X-Request-ID"]).toMatch(/^[0-9a-f-]{36}$/);
      expect(Number(resolved["X-Timestamp"])).toBeGreaterThan(0);
    });

    it("should handle empty objects", () => {
      expect(resolveHeaderTemplates({})).toEqual({});
    });

    it("should handle null/undefined", () => {
      expect(resolveHeaderTemplates(null)).toEqual({});
      expect(resolveHeaderTemplates(undefined)).toEqual({});
    });
  });

  describe("applyHeaderBlacklist", () => {
    it("should remove blacklisted headers (case-insensitive)", () => {
      const headers = {
        "X-API-Key": "secret",
        "User-Agent": "MyClient/1.0",
        "Content-Type": "application/json",
        "x-stainless-lang": "js",
      };
      const blacklist = ["user-agent", "X-Stainless-Lang"];
      const filtered = applyHeaderBlacklist(headers, blacklist);
      expect(filtered).toEqual({
        "X-API-Key": "secret",
        "Content-Type": "application/json",
      });
    });

    it("should handle empty blacklist", () => {
      const headers = { "X-Test": "value" };
      expect(applyHeaderBlacklist(headers, [])).toEqual(headers);
      expect(applyHeaderBlacklist(headers, null)).toEqual(headers);
      expect(applyHeaderBlacklist(headers, undefined)).toEqual(headers);
    });

    it("should preserve all headers if none match", () => {
      const headers = { "X-Test": "value", "Y-Test": "value2" };
      const filtered = applyHeaderBlacklist(headers, ["Z-Test"]);
      expect(filtered).toEqual(headers);
    });
  });
});
