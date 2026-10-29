// If-None-Match uses the weak comparison (RFC 9110 13.1.2). Cloudflare's edge turns a strong ETag into
// W/"..." when it compresses the body, so /v1/models never answered 304 on Workers.
import { describe, expect, it } from "vitest";
import { ifNoneMatchSatisfied, withCatalog } from "../../src/lib/modelCatalog/modelsListCache.js";

const E = '"abc123"';

describe("ifNoneMatchSatisfied", () => {
  it("strong tag matches strong ETag", () => expect(ifNoneMatchSatisfied(E, E)).toBe(true));
  it("weak tag (edge-weakened) matches strong ETag", () => expect(ifNoneMatchSatisfied(`W/${E}`, E)).toBe(true));
  it("strong tag matches a weak ETag", () => expect(ifNoneMatchSatisfied(E, `W/${E}`)).toBe(true));
  it("matches inside a list, with spaces and mixed weakness", () => {
    expect(ifNoneMatchSatisfied(`"x", W/"y" , W/${E}`, E)).toBe(true);
    expect(ifNoneMatchSatisfied(`"x","y",${E}`, E)).toBe(true);
  });
  it("* matches anything", () => expect(ifNoneMatchSatisfied("*", E)).toBe(true));
  it("mismatch does not match", () => {
    expect(ifNoneMatchSatisfied('"other"', E)).toBe(false);
    expect(ifNoneMatchSatisfied('W/"other"', E)).toBe(false);
    expect(ifNoneMatchSatisfied('"abc1234"', E)).toBe(false);
  });
  it("no header / empty header does not match", () => {
    expect(ifNoneMatchSatisfied(null, E)).toBe(false);
    expect(ifNoneMatchSatisfied(undefined, E)).toBe(false);
    expect(ifNoneMatchSatisfied("", E)).toBe(false);
  });
});

describe("withCatalog 304", () => {
  const run = (header, key) => withCatalog({
    request: { headers: new Headers(header ? { "if-none-match": header } : {}) },
    cacheKey: key,
    resolve: async () => ({ inputs: {}, fingerprint: "fp-weak" }),
    project: async () => "BODY",
  });

  it("answers notModified for strong, weak, list and *; same validator is returned", async () => {
    const first = await run(null, "etag-k");
    expect(first.notModified).toBe(false);
    expect(first.etag).toBe('"fp-weak"');
    for (const h of ['"fp-weak"', 'W/"fp-weak"', '"a", W/"fp-weak"', "*"]) {
      const r = await run(h, "etag-k");
      expect(r.notModified, h).toBe(true);
      expect(r.etag).toBe('"fp-weak"');
    }
  });
  it("returns the body on mismatch or no header", async () => {
    for (const h of ['"nope"', 'W/"nope"', null]) {
      const r = await run(h, "etag-k2");
      expect(r.notModified).toBe(false);
      expect(r.body).toBe("BODY");
    }
  });
});
