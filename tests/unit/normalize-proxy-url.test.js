import { describe, it, expect } from "vitest";
import {
  normalizeProxyUrl,
  parseHostPortUserPass,
  toProxyUrlString,
} from "../../src/lib/network/normalizeProxyUrl.js";

describe("parseHostPortUserPass", () => {
  it("parses host:port", () => {
    expect(parseHostPortUserPass("1.2.3.4:8080")).toEqual({ host: "1.2.3.4", port: "8080" });
  });

  it("parses host:port:user:pass", () => {
    expect(parseHostPortUserPass("1.2.3.4:8080:alice:s3cret")).toEqual({
      host: "1.2.3.4",
      port: "8080",
      username: "alice",
      password: "s3cret",
    });
  });

  it("keeps colons inside password (everything after 3rd colon)", () => {
    expect(parseHostPortUserPass("1.2.3.4:8080:user:pass:with:colons")).toEqual({
      host: "1.2.3.4",
      port: "8080",
      username: "user",
      password: "pass:with:colons",
    });
  });

  it("parses real provider token with = in password", () => {
    const sample =
      "5.42.206.146:8081:o2gYGm_rIqSG2zW8kSIM-FO2Oavz0iswk0aTh__Uw4MilfGNeEYUM8xOuCQkYbP0oXw609MC0WdYefYZbCn32A==:o2gYGm_rIqSG2zW8kSIM-FO2Oavz0iswk0aTh__Uw4MilfGNeEYUM8xOuCQkYbP0oXw609MC0WdYefYZbCn32A==";
    const p = parseHostPortUserPass(sample);
    expect(p.host).toBe("5.42.206.146");
    expect(p.port).toBe("8081");
    expect(p.username).toContain("o2gYGm");
    expect(p.password.endsWith("==")).toBe(true);
  });
});

describe("normalizeProxyUrl", () => {
  it("accepts full http URL", () => {
    const r = normalizeProxyUrl("http://u:p@1.2.3.4:8080");
    expect(r.ok).toBe(true);
    expect(r.url).toContain("1.2.3.4");
    expect(r.hostLabel).toBe("1.2.3.4:8080");
  });

  it("converts host:port:user:pass to http URL with encoded auth", () => {
    const r = normalizeProxyUrl("5.42.206.146:8081:alice:tok==");
    expect(r.ok).toBe(true);
    expect(r.url).toMatch(/^http:\/\//);
    expect(r.url).toContain("5.42.206.146:8081");
    expect(r.url).toContain(encodeURIComponent("tok=="));
    expect(r.hostLabel).toBe("5.42.206.146:8081");
  });

  it("converts the user's real sample", () => {
    const sample =
      "5.42.206.146:8081:o2gYGm_rIqSG2zW8kSIM-FO2Oavz0iswk0aTh__Uw4MilfGNeEYUM8xOuCQkYbP0oXw609MC0WdYefYZbCn32A==:o2gYGm_rIqSG2zW8kSIM-FO2Oavz0iswk0aTh__Uw4MilfGNeEYUM8xOuCQkYbP0oXw609MC0WdYefYZbCn32A==";
    const r = normalizeProxyUrl(sample);
    expect(r.ok).toBe(true);
    const u = new URL(r.url);
    expect(u.hostname).toBe("5.42.206.146");
    expect(u.port).toBe("8081");
    expect(decodeURIComponent(u.username)).toContain("o2gYGm");
    expect(decodeURIComponent(u.password).endsWith("==")).toBe(true);
  });

  it("accepts host:port", () => {
    const r = normalizeProxyUrl("127.0.0.1:7897");
    expect(r.ok).toBe(true);
    expect(r.url).toMatch(/127\.0\.0\.1:7897/);
  });

  it("accepts user:pass@host:port", () => {
    const r = normalizeProxyUrl("alice:s3cret@10.0.0.1:3128");
    expect(r.ok).toBe(true);
    expect(r.url).toContain("10.0.0.1:3128");
  });

  it("rejects empty", () => {
    expect(normalizeProxyUrl("").ok).toBe(false);
  });

  it("toProxyUrlString returns string or null", () => {
    expect(toProxyUrlString("1.2.3.4:80:u:p")).toMatch(/^http:/);
    expect(toProxyUrlString("")).toBeNull();
  });
});
