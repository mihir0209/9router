#!/usr/bin/env node
/**
 * End-to-end test: verify each proxy provider fetches real proxy data.
 * Run: node scripts/test-proxy-providers.mjs
 */

const PASS = "\x1b[32m✓\x1b[0m";
const FAIL = "\x1b[31m✗\x1b[0m";

const results = [];

async function testProvider(provider) {
  const start = Date.now();
  try {
    const proxies = await provider.fetch(provider.defaults);
    const elapsed = Date.now() - start;
    const samples = proxies.slice(0, 3).map(p => p.proxyUrl);
    const ok = proxies.length > 0;
    results.push({ id: provider.id, ok, count: proxies.length, samples, elapsed, error: null });
    console.log(`  ${ok ? PASS : FAIL} ${provider.name}: ${proxies.length} proxies (${elapsed}ms)`);
    if (samples.length) console.log(`    samples: ${samples.join(", ")}`);
    return ok;
  } catch (err) {
    const elapsed = Date.now() - start;
    results.push({ id: provider.id, ok: false, count: 0, samples: [], elapsed, error: err.message });
    console.log(`  ${FAIL} ${provider.name}: ERROR (${elapsed}ms) — ${err.message}`);
    return false;
  }
}

// Import registry directly (side-effect: registers providers)
const registry = await import("../src/lib/proxy/providers/registry.js");
const { getAllProviders } = registry;

// Wait for dynamic imports to settle
await new Promise(r => setTimeout(r, 500));

const providers = await getAllProviders();
console.log(`\nFound ${providers.length} registered providers:\n`);

if (providers.length === 0) {
  console.log("No providers registered. Checking for import errors...\n");
  // Try importing each provider individually to find the broken one
  const files = [
    "proxyscrape.js", "geonode.js", "thespeedx.js",
    "monosans.js", "stormsia.js", "freeproxy.js", "custom.js",
  ];
  for (const file of files) {
    try {
      await import(`../src/lib/proxy/providers/${file}`);
      console.log(`  ${PASS} ${file} imported OK`);
    } catch (err) {
      console.log(`  ${FAIL} ${file} FAILED: ${err.message}`);
    }
  }
  process.exit(1);
}

let allPassed = true;
for (const provider of providers) {
  const ok = await testProvider(provider);
  if (!ok) allPassed = false;
}

console.log("\n─── Summary ───");
const passed = results.filter(r => r.ok).length;
const failed = results.filter(r => !r.ok).length;
console.log(`  ${passed} passed, ${failed} failed, ${results.length} total`);

if (failed > 0) {
  console.log("\nFailed providers:");
  for (const r of results.filter(r => !r.ok)) {
    console.log(`  ${FAIL} ${r.id}: ${r.error || "returned 0 proxies"}`);
  }
}

console.log(allPassed ? `\n${PASS} All providers OK` : `\n${FAIL} Some providers failed`);
process.exit(allPassed ? 0 : 1);
