#!/usr/bin/env node
// Prepare an isolated, credential-free data directory for load tests.
//
//   node scripts/bench/setup-bench-db.mjs --src ~/.9router/db/data.sqlite \
//        --out /tmp/9r-bench --mock-url http://127.0.0.1:20199/v1
//
// * copies the source database (or creates an empty one when --src is omitted);
//   the source is only ever opened for reading;
// * scrubs every secret-looking field of every provider connection and replaces
//   the API key values, so nothing in the copy can authenticate to a real
//   provider, and deactivates all pre-existing connections (they stay as rows so
//   the working set has its production size);
// * adds the mock upstream as an openai-compatible node + connection and the
//   benchmark API keys; turns requireApiKey on.
//
// Prints a JSON document with the generated API keys on stdout.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { DatabaseSync } from "node:sqlite";

function arg(name, fallback = null) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : fallback;
}

const src = arg("src");
const out = path.resolve(arg("out", "/tmp/9r-bench"));
const mockUrl = arg("mock-url", "http://127.0.0.1:20199/v1");
const dbDir = path.join(out, "db");
const dbFile = path.join(dbDir, "data.sqlite");

fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(dbDir, { recursive: true });
if (src) {
  // Plain file copy; the live DB may be in WAL mode, so checkpoint through a read-only handle is not possible.
  // Copying the main file is enough for a consistent snapshot unless a WAL holds uncommitted pages.
  fs.copyFileSync(path.resolve(src.replace(/^~/, process.env.HOME || "")), dbFile);
  for (const ext of ["-wal", "-shm"]) {
    const side = path.resolve(src.replace(/^~/, process.env.HOME || "")) + ext;
    if (fs.existsSync(side)) fs.copyFileSync(side, dbFile + ext);
  }
}

const SECRET_KEY = /token|secret|key|password|cookie|credential|authorization|session|bearer/i;
function scrub(value) {
  if (Array.isArray(value)) return value.map(scrub);
  if (value && typeof value === "object") {
    const next = {};
    for (const [k, v] of Object.entries(value)) {
      next[k] = typeof v === "string" && SECRET_KEY.test(k) ? "scrubbed" : scrub(v);
    }
    return next;
  }
  return value;
}

const db = new DatabaseSync(dbFile);
db.exec("PRAGMA journal_mode = WAL");
const now = new Date().toISOString();

// Make sure the schema exists when starting from an empty file: the app creates
// it on first boot, but the inserts below need it now.
const hasSchema = db.prepare("SELECT name FROM sqlite_master WHERE name = 'providerConnections'").get();
if (!hasSchema) {
  console.error("[bench-db] no schema in the database: start the app once against this DATA_DIR, or pass --src");
  process.exit(2);
}

let scrubbed = 0;
for (const row of db.prepare("SELECT id, data FROM providerConnections").all()) {
  let data = {};
  try { data = JSON.parse(row.data); } catch { /* keep {} */ }
  db.prepare("UPDATE providerConnections SET data = ?, isActive = 0 WHERE id = ?").run(JSON.stringify(scrub(data)), row.id);
  scrubbed += 1;
}
for (const row of db.prepare("SELECT id, data FROM providerNodes").all()) {
  let data = {};
  try { data = JSON.parse(row.data); } catch { /* keep {} */ }
  db.prepare("UPDATE providerNodes SET data = ? WHERE id = ?").run(JSON.stringify(scrub(data)), row.id);
}
db.prepare("UPDATE proxyPools SET data = json_set(data, '$.proxyUrl', 'http://127.0.0.1:9')").run();
// Proxy pools could route traffic through real proxies: neutralise them too.
let keysReplaced = 0;
for (const row of db.prepare("SELECT id FROM apiKeys").all()) {
  db.prepare("UPDATE apiKeys SET key = ?, isActive = 0, byokCredentialsCiphertext = NULL WHERE id = ?").run(`sk-bench-copy-${row.id}`, row.id);
  keysReplaced += 1;
}

// Mock upstream.
const nodeId = "openai-compatible-mock";
db.prepare("DELETE FROM providerNodes WHERE id = ?").run(nodeId);
db.prepare("INSERT INTO providerNodes(id, type, name, data, createdAt, updatedAt) VALUES(?, ?, ?, ?, ?, ?)")
  .run(nodeId, "openai-compatible", "mock", JSON.stringify({ prefix: "mock", apiType: "chat", baseUrl: mockUrl }), now, now);
db.prepare("DELETE FROM providerConnections WHERE id = ?").run("conn-mock");
db.prepare("INSERT INTO providerConnections(id, provider, authType, name, email, priority, isActive, data, createdAt, updatedAt) VALUES(?, ?, ?, ?, ?, ?, 1, ?, ?, ?)")
  .run("conn-mock", nodeId, "apikey", "mock", null, 1,
    JSON.stringify({ apiKey: "sk-upstream-mock", providerSpecificData: { baseUrl: mockUrl, prefix: "mock", apiType: "chat" } }), now, now);

// Benchmark API keys.
const keyColumns = db.prepare("PRAGMA table_info(apiKeys)").all().map((c) => c.name);
function insertKey(id, key, extra = {}) {
  db.prepare("DELETE FROM apiKeys WHERE id = ?").run(id);
  const row = {
    id, key, name: id, machineId: "bench", isActive: 1, createdAt: now,
    rateLimitRequests: 0, rateLimitWindowSeconds: 60, tokenLimitDaily: 0, tokenLimitMonthly: 0,
    allowedModels: "", blockedModels: "", allowedIPs: "", blockedIPs: "", expiresAt: "", description: "bench",
    billingMode: "credits", regenerateVersion: 0, maxConcurrentRequests: 0, allowedProviders: "", blockedProviders: "",
    forceProvider: "", forceModel: "", allowedEndpoints: "", blockedEndpoints: "", metadata: "{}", updatedAt: now,
    ...extra,
  };
  const cols = Object.keys(row).filter((c) => keyColumns.includes(c));
  db.prepare(`INSERT INTO apiKeys(${cols.join(", ")}) VALUES(${cols.map(() => "?").join(", ")})`).run(...cols.map((c) => row[c]));
}
const token = () => crypto.randomBytes(12).toString("hex");
const keys = {
  plain: `sk-bench-plain-${token()}`,
  acl: `sk-bench-acl-${token()}`,
};
insertKey("bench-plain", keys.plain);
// Pins + ACL + per-key limits: exercises force routing, model/provider/endpoint
// lists, the rate-limit window and the concurrency lease on every request.
insertKey("bench-acl", keys.acl, {
  forceModel: "mock/mock-model",
  forceProvider: nodeId,
  allowedModels: "mock/*",
  blockedModels: "mock/forbidden",
  allowedEndpoints: "chat.completions,models",
  rateLimitRequests: 1_000_000_000,
  rateLimitWindowSeconds: 60,
  maxConcurrentRequests: 100_000,
});

const settings = JSON.parse(db.prepare("SELECT data FROM settings WHERE id = 1").get()?.data || "{}");
Object.assign(settings, {
  requireApiKey: true,
  cloudEnabled: false,
  tunnelEnabled: false,
  tailscaleEnabled: false,
  mitmEnabled: false,
  outboundProxyEnabled: false,
  outboundProxyUrl: "",
  codexAutoPing: { enabled: false },
  proxyscrapeProxyPool: { enabled: false },
  proxyPoolProviders: [],
  enableObservability: false,
});
db.prepare("INSERT INTO settings(id, data) VALUES(1, ?) ON CONFLICT(id) DO UPDATE SET data = excluded.data").run(JSON.stringify(settings));

const counts = {};
for (const t of ["providerConnections", "providerNodes", "proxyPools", "apiKeys", "combos", "kv"]) {
  counts[t] = db.prepare(`SELECT COUNT(*) AS n FROM ${t}`).get().n;
}
db.close();
fs.writeFileSync(path.join(out, "jwt-secret"), crypto.randomBytes(32).toString("hex"), { mode: 0o600 });
console.log(JSON.stringify({ dataDir: out, dbFile, keys, counts, scrubbedConnections: scrubbed, replacedKeys: keysReplaced, mockUrl }));
