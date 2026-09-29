import { afterEach, describe, expect, it } from "vitest";
import { assertPublicUrl, assertPublicUrlResolved } from "../../src/shared/utils/ssrfGuard.js";

// On Cloudflare Workers `node:dns` does not resolve real hostnames. Treating its
// output as authoritative rejected public sites — en.wikipedia.org and iana.org
// both came back "resolves to an internal host" while example.com happened to
// pass, so which sites failed looked arbitrary and web fetch was broken for most
// of the web.
const withRuntime = async (value, fn) => {
  const original = process.env.NINEROUTER_RUNTIME;
  if (value === undefined) delete process.env.NINEROUTER_RUNTIME;
  else process.env.NINEROUTER_RUNTIME = value;
  try { return await fn(); } finally {
    if (original === undefined) delete process.env.NINEROUTER_RUNTIME;
    else process.env.NINEROUTER_RUNTIME = original;
  }
};

describe("ssrf guard on a runtime without DNS", () => {
  afterEach(() => { delete process.env.NINEROUTER_RUNTIME; });

  it("still rejects literal private addresses on Workers", async () => {
    // Layer 1 is unaffected — this is the protection that actually matters
    // there, and the reason skipping layer 2 is tolerable.
    for (const url of ["http://127.0.0.1/", "http://169.254.169.254/", "http://10.0.0.1/"]) {
      await withRuntime("cloudflare", async () => {
        await expect(assertPublicUrlResolved(url)).rejects.toThrow();
      });
    }
  });

  it("still rejects localhost and internal suffixes on Workers", async () => {
    for (const url of ["http://localhost/", "http://foo.internal/", "http://x.local/"]) {
      await withRuntime("cloudflare", async () => {
        await expect(assertPublicUrlResolved(url)).rejects.toThrow();
      });
    }
  });

  it("still rejects IPv6 loopback and mapped addresses on Workers", async () => {
    for (const url of ["http://[::1]/", "http://[::ffff:127.0.0.1]/"]) {
      await withRuntime("cloudflare", async () => {
        await expect(assertPublicUrlResolved(url)).rejects.toThrow();
      });
    }
  });

  it("does not reject ordinary public hostnames on Workers", async () => {
    for (const url of ["https://en.wikipedia.org/wiki/Web_scraping", "https://www.iana.org/", "https://example.com"]) {
      await withRuntime("cloudflare", async () => {
        await expect(assertPublicUrlResolved(url)).resolves.toBeUndefined();
      });
    }
  });

  it("keeps the literal checks identical on every other runtime", () => {
    expect(() => assertPublicUrl("http://127.0.0.1/")).toThrow();
    expect(() => assertPublicUrl("https://example.com")).not.toThrow();
  });
});
