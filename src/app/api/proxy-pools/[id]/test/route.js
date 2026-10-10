import { NextResponse } from "next/server";
import { getProxyPoolById, updateProxyPool } from "@/models";
import { testProxyUrl } from "@/lib/network/proxyTest";
import { fetch as undiciFetch } from "undici";

// Reliable targets — if the relay can forward to any of them, it's alive.
// We try them in order and consider the relay "active" if it returns any
// HTTP response (even a 5xx from the upstream means the relay forwarded it).
const RELAY_TEST_TARGETS = [
  { host: "https://httpstat.us", path: "/200" },
  { host: "https://httpbin.org", path: "/status/200" },
  { host: "https://api.github.com", path: "/" },
];

async function testRelay(relayUrl, timeoutMs = 15000) {
  let lastError = null;

  for (const target of RELAY_TEST_TARGETS) {
    const controller = new AbortController();
    const startedAt = Date.now();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      const res = await undiciFetch(relayUrl, {
        method: "GET",
        headers: {
          "x-relay-target": target.host,
          "x-relay-path": target.path,
        },
        signal: controller.signal,
      });
      clearTimeout(timer);
      // Any HTTP response (even 5xx from upstream) means the relay is alive
      // and successfully forwarded the request. Only connection failures mean
      // the relay itself is broken.
      return {
        ok: true,
        status: res.status,
        statusText: res.statusText,
        elapsedMs: Date.now() - startedAt,
      };
    } catch (err) {
      clearTimeout(timer);
      lastError = err?.name === "AbortError" ? "Relay test timed out" : (err?.message || String(err));
      continue;
    }
  }

  return {
    ok: false,
    status: 500,
    error: lastError || "All relay test targets failed",
  };
}

// POST /api/proxy-pools/[id]/test - Test proxy pool entry
export async function POST(request, { params }) {
  try {
    const { id } = await params;
    const proxyPool = await getProxyPoolById(id);

    if (!proxyPool) {
      return NextResponse.json({ error: "Proxy pool not found" }, { status: 404 });
    }

    const result = proxyPool.type === "vercel" || proxyPool.type === "cloudflare" || proxyPool.type === "deno" || proxyPool.type === "netlify"
      ? await testRelay(proxyPool.proxyUrl)
      : await testProxyUrl({ proxyUrl: proxyPool.proxyUrl });
    const now = new Date().toISOString();

    await updateProxyPool(id, {
      testStatus: result.ok ? "active" : "error",
      lastTestedAt: now,
      lastError: result.ok ? null : (result.error || `Proxy test failed with status ${result.status}`),
      isActive: result.ok,
    });

    return NextResponse.json({
      ok: result.ok,
      status: result.status,
      statusText: result.statusText || null,
      error: result.error || null,
      elapsedMs: result.elapsedMs || 0,
      testedAt: now,
    });
  } catch (error) {
    console.log("Error testing proxy pool:", error);
    return NextResponse.json({ error: "Failed to test proxy pool" }, { status: 500 });
  }
}
