#!/usr/bin/env node
/**
 * 9Router Real-World 5GB Database Performance & Index Benchmark
 * 
 * 1. Populates a realistic 5.0 GB SQLite database using @faker-js/faker and better-sqlite3:
 *    - apiKeys (25,000 realistic keys)
 *    - providerConnections (1,000 connections across providers)
 *    - proxyPools (100 proxy pools)
 *    - combos (50 combo router configurations)
 *    - kv (15,000 model aliases & pricing entries)
 *    - usageHistory (1,200,000 token tracking records)
 *    - requestDetails (160,000 multi-turn conversation payloads with realistic ~15-25 KB payloads)
 *    - auditLog (50,000 administrative audit entries)
 * 2. Runs comprehensive benchmark suite:
 *    - Benchmark A: Index Speedup & Query Plan Verification on 5GB Scale
 *    - Benchmark B: Multi-threaded Reader Concurrency & RPS Scaling
 *    - Benchmark C: Concurrent WAL Writer Latency Under Heavy Reader Load
 * 3. Immediately cleans up the 5GB database files upon completion.
 */

const fs = require('fs');
const path = require('path');
const Database = require('better-sqlite3');
const { faker } = require('@faker-js/faker');
const { Worker, isMainThread, parentPort, workerData } = require('worker_threads');

const DB_PATH = process.env.BENCH_DB_PATH || '/var/tmp/bench_5gb.sqlite';
const TARGET_SIZE_BYTES = 5.0 * 1024 * 1024 * 1024; // 5.0 GB

// ANSI color helpers
const BOLD = '\x1b[1m';
const GREEN = '\x1b[32m';
const CYAN = '\x1b[36m';
const YELLOW = '\x1b[33m';
const RED = '\x1b[31m';
const RESET = '\x1b[0m';

function formatBytes(bytes) {
    if (bytes >= 1024 * 1024 * 1024) return (bytes / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
    if (bytes >= 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(2) + ' MB';
    if (bytes >= 1024) return (bytes / 1024).toFixed(2) + ' KB';
    return bytes + ' B';
}

function formatUs(valUs) {
    if (valUs < 1.0) return `${(valUs * 1000.0).toFixed(1)} ns`;
    if (valUs < 1000.0) return `${valUs.toFixed(2)} µs`;
    if (valUs < 1_000_000.0) return `${(valUs / 1000.0).toFixed(2)} ms`;
    return `${(valUs / 1_000_000.0).toFixed(2)} s`;
}

function computeStats(samplesMs) {
    if (!samplesMs || samplesMs.length === 0) return { min: 0, mean: 0, median: 0, p95: 0, p99: 0, max: 0 };
    const s = [...samplesMs].sort((a, b) => a - b);
    const n = s.length;
    const sum = s.reduce((acc, v) => acc + v, 0);
    const p95Idx = Math.min(n - 1, Math.ceil(0.95 * n) - 1);
    const p99Idx = Math.min(n - 1, Math.ceil(0.99 * n) - 1);
    return {
        min: s[0],
        mean: sum / n,
        median: s[Math.floor(n / 2)],
        p95: s[p95Idx],
        p99: s[p99Idx],
        max: s[n - 1]
    };
}

// ----------------------------------------------------------------------------
// Worker Thread Task (for Concurrency Benchmark)
// ----------------------------------------------------------------------------
if (!isMainThread) {
    const { dbPath, queriesPerWorker, targetKey, targetApiKeyId } = workerData;
    const workerDb = new Database(dbPath, { readonly: true });
    workerDb.pragma('query_only = ON');

    const stmtKey = workerDb.prepare('SELECT id, key, name, ownerRef FROM apiKeys WHERE key = ?');
    const stmtUsage = workerDb.prepare(`
        SELECT count(*), sum(cost), sum(promptTokens), sum(completionTokens) 
        FROM usageHistory 
        WHERE apiKeyId = ? AND timestamp >= '2026-10-01'
    `);

    const latenciesMs = [];
    for (let i = 0; i < queriesPerWorker; i++) {
        const t0 = process.hrtime.bigint();
        if (i % 2 === 0) {
            stmtKey.get(targetKey);
        } else {
            stmtUsage.get(targetApiKeyId);
        }
        const t1 = process.hrtime.bigint();
        latenciesMs.push(Number(t1 - t0) / 1_000_000.0);
    }

    workerDb.close();
    parentPort.postMessage({ latenciesMs });
    process.exit(0);
}

// ----------------------------------------------------------------------------
// Main Orchestration
// ----------------------------------------------------------------------------
async function main() {
    console.log(`\n${BOLD}${GREEN}========================================================================================${RESET}`);
    console.log(`${BOLD}${GREEN}          9Router 5.0 GB Real-World Production Database Benchmark Suite          ${RESET}`);
    console.log(`${BOLD}${GREEN}========================================================================================${RESET}`);
    console.log(`Target Database Path: ${BOLD}${DB_PATH}${RESET}`);
    console.log(`Target Target Size:   ${BOLD}5.00 GB${RESET} (realistic multi-month deployment dataset)\n`);

    // Clean up any stale file from prior interrupted runs
    if (fs.existsSync(DB_PATH)) fs.unlinkSync(DB_PATH);
    if (fs.existsSync(`${DB_PATH}-wal`)) fs.unlinkSync(`${DB_PATH}-wal`);
    if (fs.existsSync(`${DB_PATH}-shm`)) fs.unlinkSync(`${DB_PATH}-shm`);

    const db = new Database(DB_PATH);
    db.pragma('journal_mode = WAL');
    db.pragma('synchronous = OFF'); // Accelerated for synthetic generation
    db.pragma('busy_timeout = 10000');

    // 1. Initialize exact 9Router Schema
    console.log(`${BOLD}${CYAN}[Phase 1/4] Initializing exact 9Router native schema...${RESET}`);
    db.exec(`
        CREATE TABLE IF NOT EXISTS settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            data TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS providerConnections (
            id TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            authType TEXT NOT NULL,
            name TEXT,
            email TEXT,
            priority INTEGER,
            isActive INTEGER DEFAULT 1,
            data TEXT NOT NULL,
            createdAt TEXT NOT NULL,
            updatedAt TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS apiKeys (
            id TEXT PRIMARY KEY,
            key TEXT UNIQUE NOT NULL,
            name TEXT,
            machineId TEXT,
            isActive INTEGER DEFAULT 1,
            createdAt TEXT NOT NULL,
            rateLimitRequests INTEGER DEFAULT 0,
            rateLimitWindowSeconds INTEGER DEFAULT 60,
            tokenLimitDaily INTEGER DEFAULT 0,
            tokenLimitMonthly INTEGER DEFAULT 0,
            allowedModels TEXT DEFAULT '',
            blockedModels TEXT DEFAULT '',
            allowedIPs TEXT DEFAULT '',
            blockedIPs TEXT DEFAULT '',
            expiresAt TEXT DEFAULT '',
            description TEXT DEFAULT '',
            ownerRef TEXT,
            maxConcurrentRequests INTEGER DEFAULT 0,
            allowedProviders TEXT DEFAULT '',
            blockedProviders TEXT DEFAULT '',
            forceProvider TEXT DEFAULT '',
            forceModel TEXT DEFAULT '',
            allowedEndpoints TEXT DEFAULT '',
            blockedEndpoints TEXT DEFAULT '',
            budgetUsd REAL,
            budgetResetHours REAL,
            budgetWindowStartedAt TEXT,
            billingMode TEXT DEFAULT 'credits',
            byokProvider TEXT,
            regenerateVersion INTEGER DEFAULT 0,
            metadata TEXT DEFAULT '{}',
            updatedAt TEXT
        );

        CREATE TABLE IF NOT EXISTS combos (
            id TEXT PRIMARY KEY,
            name TEXT UNIQUE NOT NULL,
            kind TEXT,
            models TEXT NOT NULL,
            strategy TEXT DEFAULT 'fallback',
            matchRules TEXT DEFAULT '[]',
            customPrompt TEXT DEFAULT '',
            createdAt TEXT NOT NULL,
            updatedAt TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS proxyPools (
            id TEXT PRIMARY KEY,
            isActive INTEGER DEFAULT 1,
            testStatus TEXT,
            data TEXT NOT NULL,
            createdAt TEXT NOT NULL,
            updatedAt TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS kv (
            scope TEXT NOT NULL,
            key TEXT NOT NULL,
            value TEXT NOT NULL,
            PRIMARY KEY (scope, key)
        );

        CREATE TABLE IF NOT EXISTS usageHistory (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL,
            provider TEXT,
            model TEXT,
            connectionId TEXT,
            apiKey TEXT,
            apiKeyId TEXT,
            endpoint TEXT,
            promptTokens INTEGER DEFAULT 0,
            completionTokens INTEGER DEFAULT 0,
            cachedTokens INTEGER DEFAULT 0,
            cost REAL DEFAULT 0,
            status TEXT,
            tokens TEXT,
            meta TEXT
        );

        CREATE TABLE IF NOT EXISTS requestDetails (
            id TEXT PRIMARY KEY,
            timestamp TEXT NOT NULL,
            provider TEXT,
            model TEXT,
            connectionId TEXT,
            status TEXT,
            data TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS auditLog (
            id TEXT PRIMARY KEY,
            timestamp TEXT NOT NULL,
            actor TEXT NOT NULL,
            action TEXT NOT NULL,
            resourceType TEXT NOT NULL,
            resourceId TEXT,
            ownerRef TEXT,
            requestId TEXT,
            sourceIp TEXT,
            outcome TEXT NOT NULL,
            before TEXT,
            after TEXT,
            metadata TEXT
        );
    `);

    // 2. Population with Faker
    console.log(`${BOLD}${CYAN}[Phase 2/4] Populating 5.0 GB of realistic multi-month data using Faker...${RESET}`);
    const tGenStart = Date.now();

    // A. Populate Settings & Combos
    db.prepare('INSERT INTO settings VALUES (1, ?)').run(JSON.stringify({ requireLogin: true, defaultModel: 'gpt-4o' }));

    const insertCombo = db.prepare('INSERT INTO combos VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)');
    for (let i = 0; i < 50; i++) {
        insertCombo.run(
            `combo-${i}`, `combo-preset-${i}`, 'chat', JSON.stringify(['gpt-4o', 'claude-3-5-sonnet', 'gemini-1.5-pro']),
            'fallback', '[]', '', new Date().toISOString(), new Date().toISOString()
        );
    }

    // B. Populate Proxy Pools
    const insertPool = db.prepare('INSERT INTO proxyPools VALUES (?, ?, ?, ?, ?, ?)');
    for (let i = 0; i < 100; i++) {
        insertPool.run(
            `pool-${i}`, i % 4 !== 0 ? 1 : 0, 'active', JSON.stringify({ url: `http://proxy-${i}.internal:8080`, timeout: 5000 }),
            new Date().toISOString(), new Date().toISOString()
        );
    }

    // C. Populate Provider Connections (1,000 rows)
    const insertConn = db.prepare('INSERT INTO providerConnections VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)');
    const connIds = [];
    const providers = ['openai', 'anthropic', 'gemini', 'deepseek', 'groq', 'together', 'mistral'];
    const insertConnTx = db.transaction(() => {
        for (let i = 0; i < 1000; i++) {
            const cid = `conn-${faker.string.alphanumeric(10)}`;
            connIds.push(cid);
            insertConn.run(
                cid,
                providers[i % providers.length],
                'apiKey',
                faker.company.name(),
                faker.internet.email(),
                i % 100, // priority
                i % 5 !== 0 ? 1 : 0, // 80% active
                JSON.stringify({ apiKey: `sk-live-${faker.string.alphanumeric(32)}`, baseUrl: 'https://api.openai.com/v1' }),
                new Date(Date.now() - i * 3600_000).toISOString(),
                new Date().toISOString()
            );
        }
    });
    insertConnTx();

    // D. Populate API Keys (25,000 keys)
    console.log('  -> Generating 25,000 realistic API keys...');
    const insertKey = db.prepare(`
        INSERT INTO apiKeys (id, key, name, ownerRef, isActive, createdAt, rateLimitRequests, rateLimitWindowSeconds, tokenLimitDaily, metadata)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `);
    const keyIds = [];
    let benchmarkTargetKey = '';
    let benchmarkTargetApiKeyId = '';

    const insertKeyTx = db.transaction(() => {
        for (let i = 0; i < 25000; i++) {
            const kid = `key-${faker.string.alphanumeric(12)}`;
            const rawKey = `sk-live-${faker.string.alphanumeric(36)}`;
            keyIds.push(kid);
            if (i === 12500) {
                benchmarkTargetKey = rawKey;
                benchmarkTargetApiKeyId = kid;
            }
            insertKey.run(
                kid,
                rawKey,
                `Project Key ${faker.commerce.productName()}`,
                `tenant-${i % 200}`,
                i % 10 !== 0 ? 1 : 0,
                faker.date.recent({ days: 90 }).toISOString(),
                120,
                60,
                10_000_000,
                JSON.stringify({ tier: 'enterprise', org: faker.company.name() })
            );
        }
    });
    insertKeyTx();

    // E. Populate KV Scopes (15,000 custom models & pricing records)
    console.log('  -> Populating 15,000 KV scope entries...');
    const insertKv = db.prepare('INSERT INTO kv VALUES (?, ?, ?)');
    const insertKvTx = db.transaction(() => {
        for (let i = 0; i < 15000; i++) {
            const scope = i % 3 === 0 ? 'modelAliases' : (i % 3 === 1 ? 'pricing' : 'customModels');
            insertKv.run(scope, `model-route-${i}`, JSON.stringify({ destination: 'openai/gpt-4o', multiplier: 1.0, tags: ['fast', 'stable'] }));
        }
    });
    insertKvTx();

    // F. Populate Usage History (1,200,000 rows, batch size 20,000)
    console.log('  -> Streaming 1,200,000 token tracking records into usageHistory...');
    const insertUsage = db.prepare(`
        INSERT INTO usageHistory (timestamp, provider, model, connectionId, apiKey, apiKeyId, endpoint, promptTokens, completionTokens, cost, status)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `);
    const totalUsageRows = 1_200_000;
    const batchSizeUsage = 25_000;
    for (let offset = 0; offset < totalUsageRows; offset += batchSizeUsage) {
        const usageTx = db.transaction(() => {
            for (let i = 0; i < batchSizeUsage; i++) {
                const kIdx = (offset + i) % keyIds.length;
                const cIdx = (offset + i) % connIds.length;
                const promptTok = faker.number.int({ min: 100, max: 4000 });
                const compTok = faker.number.int({ min: 50, max: 1500 });
                const cost = (promptTok * 0.000005) + (compTok * 0.000015);

                insertUsage.run(
                    faker.date.recent({ days: 90 }).toISOString(),
                    providers[i % providers.length],
                    'gpt-4o',
                    connIds[cIdx],
                    `sk-live-masked-${kIdx}`,
                    keyIds[kIdx],
                    '/v1/chat/completions',
                    promptTok,
                    compTok,
                    cost,
                    'ok'
                );
            }
        });
        usageTx();
        process.stdout.write(`\r     Usage history progress: ${(offset + batchSizeUsage).toLocaleString()} / ${totalUsageRows.toLocaleString()} rows`);
    }
    console.log();

    // G. Populate Request Details (160,000 multi-turn conversation payloads with ~15-25 KB payloads)
    console.log('  -> Populating large requestDetails payloads to reach 5.0 GB total volume...');
    const insertDetail = db.prepare('INSERT INTO requestDetails VALUES (?, ?, ?, ?, ?, ?, ?)');

    // Generate realistic multi-paragraph JSON conversations using Faker
    const basePayloads = [];
    for (let p = 0; p < 20; p++) {
        const conv = {
            id: faker.string.uuid(),
            model: 'gpt-4o',
            messages: [
                { role: 'system', content: `System prompt instruction: ${faker.lorem.paragraphs(5)}` },
                { role: 'user', content: `User query: ${faker.lorem.paragraphs(8)}\n\`\`\`rust\n// Code snippet\n${faker.lorem.lines(10)}\n\`\`\`` },
                { role: 'assistant', content: `AI response: ${faker.lorem.paragraphs(12)}`, tool_calls: [
                    { id: faker.string.alphanumeric(8), type: 'function', function: { name: 'execute_query', arguments: JSON.stringify({ sql: faker.lorem.sentence() }) } }
                ]},
                { role: 'tool', content: `Tool execution result: ${faker.lorem.paragraphs(6)}` },
                { role: 'assistant', content: `Final synthesis: ${faker.lorem.paragraphs(15)}` }
            ],
            response: {
                id: `chatcmpl-${faker.string.alphanumeric(16)}`,
                choices: [{ finish_reason: 'stop', message: { role: 'assistant', content: faker.lorem.paragraphs(10) } }],
                usage: { prompt_tokens: 1850, completion_tokens: 920, total_tokens: 2770 }
            },
            client_ip: faker.internet.ip(),
            headers: { 'user-agent': '9Router-Client/0.6.0', 'x-request-id': faker.string.uuid() }
        };
        basePayloads.push(JSON.stringify(conv));
    }

    let detailCount = 0;
    const batchSizeDetail = 5_000;
    while (true) {
        const detailTx = db.transaction(() => {
            for (let i = 0; i < batchSizeDetail; i++) {
                const cIdx = (detailCount + i) % connIds.length;
                const pIdx = (detailCount + i) % basePayloads.length;
                insertDetail.run(
                    `req_${faker.string.alphanumeric(14)}_${detailCount + i}`,
                    faker.date.recent({ days: 90 }).toISOString(),
                    providers[i % providers.length],
                    'gpt-4o',
                    connIds[cIdx],
                    '200',
                    basePayloads[pIdx]
                );
            }
        });
        detailTx();
        detailCount += batchSizeDetail;

        // Check current file size
        const currentSize = fs.statSync(DB_PATH).size;
        process.stdout.write(`\r     Request details progress: ${detailCount.toLocaleString()} rows | Database size: ${formatBytes(currentSize)} / ${formatBytes(TARGET_SIZE_BYTES)}`);
        if (currentSize >= TARGET_SIZE_BYTES) {
            break;
        }
    }
    console.log();

    // Checkpoint WAL to consolidate all pages into main SQLite database file
    console.log('  -> Consolidating WAL journal pages into primary 5GB B-Tree file...');
    db.pragma('synchronous = NORMAL');
    db.pragma('wal_checkpoint(TRUNCATE)');

    const finalSize = fs.statSync(DB_PATH).size;
    const genDurationSec = ((Date.now() - tGenStart) / 1000).toFixed(1);
    console.log(`${BOLD}${GREEN}✔ 5.0 GB Database populated successfully! Final Size: ${formatBytes(finalSize)} in ${genDurationSec}s.${RESET}\n`);

    // ------------------------------------------------------------------------
    // Benchmark A: Real 5GB Database Index Impact
    // ------------------------------------------------------------------------
    console.log(`${BOLD}${CYAN}[Phase 3/4] Running Index Speedup & Query Plan Benchmarks on 5.0 GB Database...${RESET}`);
    console.log(`\n${BOLD}${CYAN}=== Benchmark A: 5.0 GB SQLite Database Index Evaluation ===${RESET}`);
    console.log(`${'Query Target / Table'.padEnd(38)} | ${'No Index (Scan)'.padStart(16)} | ${'With Index'.padStart(14)} | ${'Speedup'.padStart(10)} | ${'Query Plan on 5GB DB'.padEnd(28)}`);
    console.log('-'.repeat(115));

    const indexBenchmarks = [
        {
            label: 'apiKeys: auth key seek (25k keys)',
            query: 'SELECT id, key, name, ownerRef FROM apiKeys WHERE key = ?',
            param: benchmarkTargetKey,
            createIndexSql: 'CREATE INDEX IF NOT EXISTS idx_ak_key ON apiKeys(key);',
            dropIndexSql: 'DROP INDEX IF EXISTS idx_ak_key;'
        },
        {
            label: 'providerConnections: priority routing',
            query: 'SELECT id, provider, priority, data FROM providerConnections WHERE provider = ? AND isActive = 1 ORDER BY priority DESC LIMIT 1',
            param: 'openai',
            createIndexSql: 'CREATE INDEX IF NOT EXISTS idx_pc_provider_priority ON providerConnections(provider, isActive, priority DESC);',
            dropIndexSql: 'DROP INDEX IF EXISTS idx_pc_provider_priority;'
        },
        {
            label: 'proxyPools: active health filtering',
            query: 'SELECT id, data FROM proxyPools WHERE isActive = 1',
            param: null,
            createIndexSql: 'CREATE INDEX IF NOT EXISTS idx_pp_active ON proxyPools(isActive);',
            dropIndexSql: 'DROP INDEX IF EXISTS idx_pp_active;'
        },
        {
            label: 'usageHistory: daily cost sum (1.2M rows)',
            query: "SELECT count(*), sum(cost), sum(promptTokens), sum(completionTokens) FROM usageHistory WHERE apiKeyId = ? AND timestamp >= '2026-10-01'",
            param: benchmarkTargetApiKeyId,
            createIndexSql: 'CREATE INDEX IF NOT EXISTS idx_uh_key_ts ON usageHistory(apiKeyId, timestamp DESC);',
            dropIndexSql: 'DROP INDEX IF EXISTS idx_uh_key_ts;'
        },
        {
            label: 'requestDetails: inspect per-connection (3GB+)',
            query: "SELECT id, timestamp, status FROM requestDetails WHERE connectionId = ? AND timestamp >= '2026-10-01' ORDER BY timestamp DESC LIMIT 20",
            param: connIds[10],
            createIndexSql: 'CREATE INDEX IF NOT EXISTS idx_rd_key_ts ON requestDetails(connectionId, timestamp DESC);',
            dropIndexSql: 'DROP INDEX IF EXISTS idx_rd_key_ts;'
        }
    ];

    const benchmarkAResults = [];

    for (const b of indexBenchmarks) {
        // Ensure index is DROPPED first for baseline scan
        try { db.exec(b.dropIndexSql); } catch (e) {}

        const stmtScan = db.prepare(b.query);
        const planScan = db.prepare(`EXPLAIN QUERY PLAN ${b.query}`).all(b.param !== null ? b.param : []);
        const planScanDesc = planScan[0]?.detail || 'SCAN TABLE';

        // Measure scan latency (fewer iterations on huge tables to respect runtime)
        const itersScan = b.label.includes('3GB+') ? 5 : (b.label.includes('1.2M') ? 10 : 100);
        const t0 = process.hrtime.bigint();
        for (let i = 0; i < itersScan; i++) {
            if (b.param !== null) stmtScan.all(b.param);
            else stmtScan.all();
        }
        const tScanTotalMs = Number(process.hrtime.bigint() - t0) / 1_000_000.0;
        const scanUs = (tScanTotalMs * 1000.0) / itersScan;

        // Build index
        const tIndexStart = Date.now();
        db.exec(b.createIndexSql);
        const indexBuildMs = Date.now() - tIndexStart;

        const stmtIndexed = db.prepare(b.query);
        const planIndexed = db.prepare(`EXPLAIN QUERY PLAN ${b.query}`).all(b.param !== null ? b.param : []);
        const planIndexedDesc = planIndexed[0]?.detail || 'USING INDEX';

        // Measure indexed latency
        const itersIndexed = 500;
        const t1 = process.hrtime.bigint();
        for (let i = 0; i < itersIndexed; i++) {
            if (b.param !== null) stmtIndexed.all(b.param);
            else stmtIndexed.all();
        }
        const tIndexedTotalMs = Number(process.hrtime.bigint() - t1) / 1_000_000.0;
        const indexedUs = (tIndexedTotalMs * 1000.0) / itersIndexed;

        const speedup = scanUs / Math.max(indexedUs, 0.001);
        const planSummary = planIndexedDesc.includes('USING INDEX') || planIndexedDesc.includes('COVERING') ? 'USING INDEX' : 'SCAN TABLE';

        console.log(`${b.label.padEnd(38)} | ${formatUs(scanUs).padStart(16)} | ${formatUs(indexedUs).padStart(14)} | ${(speedup.toFixed(1) + 'x').padStart(10)} | ${planSummary.padEnd(28)}`);
        benchmarkAResults.push({
            label: b.label,
            scanUs,
            indexedUs,
            speedup,
            planScanDesc,
            planIndexedDesc,
            indexBuildMs
        });
    }

    // ------------------------------------------------------------------------
    // Benchmark B: Multi-threaded Reader Concurrency on 5GB Database
    // ------------------------------------------------------------------------
    console.log(`\n${BOLD}${CYAN}=== Benchmark B: High-Concurrency Reader Scaling on 5.0 GB Database (WAL Mode) ===${RESET}`);
    console.log(`${'Concurrency Level'.padEnd(36)} | ${'Reqs'.padStart(6)} | ${'Min'.padStart(10)} | ${'Mean'.padStart(10)} | ${'P50'.padStart(10)} | ${'P95'.padStart(10)} | ${'Max'.padStart(10)} | ${'RPS'.padStart(9)}`);
    console.log('-'.repeat(115));

    const concurrencyTiers = [1, 5, 10, 25, 50];
    const benchmarkBResults = [];

    for (const c of concurrencyTiers) {
        const totalRequests = 500;
        const queriesPerWorker = Math.max(5, Math.floor(totalRequests / c));

        const tStart = Date.now();
        const workerPromises = [];
        for (let w = 0; w < c; w++) {
            workerPromises.push(new Promise((resolve, reject) => {
                const worker = new Worker(__filename, {
                    env: process.env,
                    workerData: {
                        dbPath: DB_PATH,
                        queriesPerWorker,
                        targetKey: benchmarkTargetKey,
                        targetApiKeyId: benchmarkTargetApiKeyId
                    }
                });
                worker.on('message', (msg) => resolve(msg.latenciesMs));
                worker.on('error', reject);
            }));
        }

        const allLatenciesNested = await Promise.all(workerPromises);
        const tDurationSec = (Date.now() - tStart) / 1000.0;
        const flatLatencies = allLatenciesNested.flat();

        const stats = computeStats(flatLatencies);
        const rps = flatLatencies.length / Math.max(tDurationSec, 0.0001);

        console.log(
            `${`Concurrency ${c} worker threads`.padEnd(36)} | ` +
            `${String(flatLatencies.length).padStart(6)} | ` +
            `${((stats.min * 1000).toFixed(1) + ' µs').padStart(10)} | ` +
            `${((stats.mean * 1000).toFixed(1) + ' µs').padStart(10)} | ` +
            `${((stats.median * 1000).toFixed(1) + ' µs').padStart(10)} | ` +
            `${((stats.p95 * 1000).toFixed(1) + ' µs').padStart(10)} | ` +
            `${(stats.max >= 1.0 ? stats.max.toFixed(2) + ' ms' : (stats.max * 1000).toFixed(1) + ' µs').padStart(10)} | ` +
            `${rps.toFixed(1).padStart(9)}`
        );

        benchmarkBResults.push({
            concurrency: c,
            requests: flatLatencies.length,
            stats,
            rps
        });
    }

    // ------------------------------------------------------------------------
    // Benchmark C: Concurrent Writer Latency Under Heavy Reader Load (5GB DB)
    // ------------------------------------------------------------------------
    console.log(`\n${BOLD}${CYAN}=== Benchmark C: Concurrent WAL Writer Latency with 25 Background Readers ===${RESET}`);
    console.log(`${'Operation / Condition'.padEnd(45)} | ${'Ops'.padStart(6)} | ${'Min'.padStart(10)} | ${'Mean'.padStart(10)} | ${'P50'.padStart(10)} | ${'P95'.padStart(10)} | ${'Max'.padStart(10)}`);
    console.log('-'.repeat(105));

    // Spawn 25 background readers continuously reading the 5GB DB
    let stopReaders = false;
    const bgReaderPromises = [];
    for (let w = 0; w < 25; w++) {
        bgReaderPromises.push(new Promise((resolve) => {
            const worker = new Worker(__filename, {
                env: process.env,
                workerData: {
                    dbPath: DB_PATH,
                    queriesPerWorker: 1000,
                    targetKey: benchmarkTargetKey,
                    targetApiKeyId: benchmarkTargetApiKeyId
                }
            });
            worker.on('message', () => resolve());
            worker.on('error', () => resolve());
        }));
    }

    // Measure writer latency while readers are actively querying
    const writeStmt = db.prepare(`
        INSERT INTO usageHistory (timestamp, provider, model, connectionId, apiKey, apiKeyId, endpoint, promptTokens, completionTokens, cost, status)
        VALUES (?, 'openai', 'gpt-4o', 'conn-write-bench', 'sk-write-test', 'key-write-test', '/v1/chat/completions', 100, 50, 0.001, 'ok')
    `);

    const writeSamplesMs = [];
    for (let i = 0; i < 200; i++) {
        const t0 = process.hrtime.bigint();
        writeStmt.run(new Date().toISOString());
        const t1 = process.hrtime.bigint();
        writeSamplesMs.push(Number(t1 - t0) / 1_000_000.0);
    }

    await Promise.all(bgReaderPromises);

    const wStats = computeStats(writeSamplesMs);
    console.log(
        `${'Usage Record Write (WAL with 25 Readers)'.padEnd(45)} | ` +
        `${String(writeSamplesMs.length).padStart(6)} | ` +
        `${((wStats.min * 1000).toFixed(1) + ' µs').padStart(10)} | ` +
        `${((wStats.mean * 1000).toFixed(1) + ' µs').padStart(10)} | ` +
        `${((wStats.median * 1000).toFixed(1) + ' µs').padStart(10)} | ` +
        `${((wStats.p95 * 1000).toFixed(1) + ' µs').padStart(10)} | ` +
        `${(wStats.max >= 1.0 ? wStats.max.toFixed(2) + ' ms' : (wStats.max * 1000).toFixed(1) + ' µs').padStart(10)}`
    );

    db.close();

    // ------------------------------------------------------------------------
    // Phase 4: Clean up 5GB file immediately
    // ------------------------------------------------------------------------
    console.log(`\n${BOLD}${CYAN}[Phase 4/4] Cleaning up 5.0 GB database files immediately...${RESET}`);
    if (fs.existsSync(DB_PATH)) fs.unlinkSync(DB_PATH);
    if (fs.existsSync(`${DB_PATH}-wal`)) fs.unlinkSync(`${DB_PATH}-wal`);
    if (fs.existsSync(`${DB_PATH}-shm`)) fs.unlinkSync(`${DB_PATH}-shm`);
    console.log(`${BOLD}${GREEN}✔ Cleanup complete! Disk space restored.${RESET}\n`);

    // Output JSON results for report compilation
    const benchmarkSummary = {
        database_size_bytes: finalSize,
        database_size_gb: (finalSize / (1024 * 1024 * 1024)).toFixed(2),
        index_benchmarks: benchmarkAResults,
        concurrency_benchmarks: benchmarkBResults,
        writer_benchmark: wStats
    };

    fs.writeFileSync('scripts/bench_5gb_results.json', JSON.stringify(benchmarkSummary, null, 2));
    console.log('Results saved to scripts/bench_5gb_results.json\n');
}

main().catch((err) => {
    console.error(`${RED}Fatal error during 5GB benchmark:${RESET}`, err);
    // Cleanup even on error
    try {
        if (fs.existsSync(DB_PATH)) fs.unlinkSync(DB_PATH);
        if (fs.existsSync(`${DB_PATH}-wal`)) fs.unlinkSync(`${DB_PATH}-wal`);
        if (fs.existsSync(`${DB_PATH}-shm`)) fs.unlinkSync(`${DB_PATH}-shm`);
    } catch (e) {}
    process.exit(1);
});
