import { describe, expect, it } from "vitest";
import tinyfish from "../../open-sse/providers/registry/tinyfish.js";
import { buildSearchRequest } from "../../open-sse/handlers/search/callers.js";
import { normalizeSearchResponse } from "../../open-sse/handlers/search/normalizers.js";

// TinyFish is registered, and both halves of the adapter pair exist. The pair
// is two hand-maintained maps with no cross-check, so a provider present in one
// and missing from the other fails silently: the request goes out, the response
// comes back, and normalizeSearchResponse returns an empty result set with a
// 200. That is exactly the failure this test exists to prevent.
describe("tinyfish provider", () => {
  it("is registered for both web search and web fetch", () => {
    expect(tinyfish.id).toBe("tinyfish");
    expect(tinyfish.serviceKinds).toEqual(["webSearch", "webFetch"]);
    expect(tinyfish.searchConfig.baseUrl).toBe("https://api.search.tinyfish.ai");
    expect(tinyfish.fetchConfig.baseUrl).toBe("https://api.fetch.tinyfish.ai");
  });

  it("declares x-api-key auth, not bearer", () => {
    // TinyFish does not use Authorization: Bearer. A default would send the key
    // in the wrong header and fail as 401.
    expect(tinyfish.searchConfig.authHeader).toBe("x-api-key");
    expect(tinyfish.fetchConfig.authHeader).toBe("x-api-key");
  });

  it("is free on every plan", () => {
    expect(tinyfish.searchConfig.costPerQuery).toBe(0);
    expect(tinyfish.fetchConfig.costPerQuery).toBe(0);
  });

  it("builds a GET with X-API-Key and domain_type", () => {
    const { url, init } = buildSearchRequest({ id: tinyfish.id, ...tinyfish.searchConfig }, {
      query: "web automation",
      searchType: "news",
      maxResults: 5,
      token: "sk-test",
    });
    expect(init.method).toBe("GET");
    expect(url).toContain("query=web+automation");
    expect(url).toContain("domain_type=news");
    expect(init.headers["X-API-Key"]).toBe("sk-test");
  });

  it("maps the client time range onto recency_minutes", () => {
    const { url } = buildSearchRequest({ id: tinyfish.id, ...tinyfish.searchConfig }, { query: "x", searchType: "web", maxResults: 5, token: "t", timeRange: "week" });
    expect(url).toContain("recency_minutes=10080");
  });

  it("has a normalizer registered for the builder", () => {
    // A builder with no normalizer returns 200 and zero results.
    const raw = { results: [{ position: 1, title: "T", url: "https://e.com", snippet: "S" }], total_results: 1 };
    const out = normalizeSearchResponse("tinyfish", raw, "q", "web");
    expect(out.results).toHaveLength(1);
    expect(out.results[0].title).toBe("T");
    expect(out.results[0].citation.provider).toBe("tinyfish");
  });

  it("normalizes into the shared SearchResult shape", () => {
    const raw = {
      results: [{ position: 1, site_name: "e.com", title: "T", url: "https://e.com/a", snippet: "S" }],
      total_results: 1,
    };
    const r = normalizeSearchResponse("tinyfish", raw, "q", "web").results[0];
    for (const key of ["title", "url", "display_url", "snippet", "position", "score", "published_at", "content", "metadata", "citation"]) {
      expect(Object.keys(r)).toContain(key);
    }
    expect(r.position).toBe(1);
  });

  it("keeps the upstream rank rather than the array index", () => {
    // Page two's first hit is rank 11 upstream, not 1.
    const raw = { results: [{ position: 11, title: "T", url: "https://e.com", snippet: "S" }] };
    expect(normalizeSearchResponse("tinyfish", raw, "q", "web").results[0].position).toBe(11);
  });

  it("survives a malformed upstream body without throwing", () => {
    expect(normalizeSearchResponse("tinyfish", {}, "q", "web")).toEqual({ results: [], totalResults: null });
    expect(normalizeSearchResponse("tinyfish", { results: "nope" }, "q", "web").results).toEqual([]);
  });
});
