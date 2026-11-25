/**
 * High-performance WebAssembly binding for 9Router on Cloudflare Workers edge.
 * Wraps GatewayCoordinator, CoordinatorCoalescer, and fast-path route resolution.
 */
export class EdgeWasm {
  constructor(instance) {
    this.instance = instance;
    this.exports = instance.exports;
    this.memory = this.exports.memory;
    this.textEncoder = new TextEncoder();
    this.textDecoder = new TextDecoder();
    this._encodedMethods = new Map();
    this.exports.edge_wasm_init();
  }

  /**
   * Synchronously instantiates EdgeWasm from a pre-compiled WebAssembly.Module
   * (e.g. a Cloudflare Workers CompiledWasm ES module import) with zero FS access.
   * @param {WebAssembly.Module} wasmModule
   * @returns {EdgeWasm}
   */
  static fromModule(wasmModule) {
    if (!(wasmModule instanceof WebAssembly.Module)) {
      throw new TypeError("EdgeWasm.fromModule expects a WebAssembly.Module");
    }
    const instance = new WebAssembly.Instance(wasmModule, {});
    return new EdgeWasm(instance);
  }

  /**
   * Loads and instantiates the Edge WebAssembly module.
   * Zero-FS when passed a WebAssembly.Module, Uint8Array, or ArrayBuffer.
   * Dynamically imports node:fs/node:path only when loading from disk in Node.
   * @param {string|Uint8Array|ArrayBuffer|WebAssembly.Module} [source]
   */
  static async load(source) {
    if (source instanceof WebAssembly.Module) {
      return EdgeWasm.fromModule(source);
    }

    let wasmBytes;
    if (source instanceof Uint8Array || source instanceof ArrayBuffer) {
      wasmBytes = source;
    } else {
      const [{ readFileSync, existsSync }, { join, dirname }, { fileURLToPath }] = await Promise.all([
        import("node:fs"),
        import("node:path"),
        import("node:url"),
      ]);
      if (typeof source === "string" && existsSync(source)) {
        wasmBytes = readFileSync(source);
      } else {
        const selfDir = dirname(fileURLToPath(import.meta.url));
        const candidatePaths = [
          join(selfDir, "ninerouter_edge_wasm.wasm"),
          join(process.cwd(), "src", "cloudflare", "ninerouter_edge_wasm.wasm"),
          join(process.cwd(), "rust", "edge-wasm", "target", "wasm32-unknown-unknown", "release", "ninerouter_edge_wasm.wasm"),
          join(process.cwd(), "rust", "wasm", "target", "wasm32-unknown-unknown", "release", "ninerouter_edge_wasm.wasm"),
          join(process.cwd(), ".open-next", "ninerouter_edge_wasm.wasm"),
          join(process.cwd(), ".open-next", "assets", "ninerouter_edge_wasm.wasm"),
        ];
        const found = candidatePaths.find((p) => existsSync(p));
        if (!found) {
          throw new Error(`Edge WASM binary not found in candidate paths: ${candidatePaths.join(", ")}`);
        }
        wasmBytes = readFileSync(found);
      }
    }

    const module = await WebAssembly.compile(wasmBytes);
    return EdgeWasm.fromModule(module);
  }

  _allocBytes(bytes) {
    if (!bytes || !bytes.length) return { ptr: 0, len: 0 };
    const ptr = this.exports.wasm_alloc(bytes.length);
    new Uint8Array(this.memory.buffer, ptr, bytes.length).set(bytes);
    return { ptr, len: bytes.length };
  }

  _allocString(str) {
    if (!str) return { ptr: 0, len: 0 };
    return this._allocBytes(this.textEncoder.encode(str));
  }

  _allocMethod(method) {
    let bytes = this._encodedMethods.get(method);
    if (!bytes) {
      bytes = this.textEncoder.encode(method);
      this._encodedMethods.set(method, bytes);
    }
    return this._allocBytes(bytes);
  }

  _readResult() {
    const ptr = this.exports.edge_wasm_get_result_ptr();
    const len = this.exports.edge_wasm_get_result_len();
    if (ptr === 0 || len === 0) return null;
    const bytes = new Uint8Array(this.memory.buffer, ptr, len);
    const jsonStr = this.textDecoder.decode(bytes);
    return JSON.parse(jsonStr);
  }

  call(serviceId, method, payload = {}) {
    const methodAlloc = this._allocMethod(method);
    const payloadAlloc = this._allocString(JSON.stringify(payload));
    try {
      this.exports.edge_wasm_rpc(
        serviceId,
        methodAlloc.ptr,
        methodAlloc.len,
        payloadAlloc.ptr,
        payloadAlloc.len
      );
      return this._readResult();
    } finally {
      if (methodAlloc.ptr) this.exports.wasm_dealloc(methodAlloc.ptr, methodAlloc.len);
      if (payloadAlloc.ptr) this.exports.wasm_dealloc(payloadAlloc.ptr, payloadAlloc.len);
    }
  }

  // --- GatewayCoordinator Contract Methods ---

  checkRateLimit({ keyId, limit = 100, windowMs = 60000, now = Date.now() }) {
    return this.call(1, "checkRateLimit", { keyId, limit, windowMs, now });
  }

  peekRateLimit({ keyId, limit = 100, windowMs = 60000, now = Date.now() }) {
    return this.call(1, "peekRateLimit", { keyId, limit, windowMs, now });
  }

  beginRequest({ keyId, maxConcurrent = 10, now = Date.now() }) {
    return this.call(1, "beginRequest", { keyId, maxConcurrent, now });
  }

  renewRequest({ keyId, leaseId, ttlMs = 120000, now = Date.now() }) {
    return this.call(1, "renewRequest", { keyId, leaseId, ttlMs, now });
  }

  endRequest({ keyId, leaseId }) {
    return this.call(1, "endRequest", { keyId, leaseId });
  }

  setLock({ provider, connectionId, model, untilMs, status = 429, reason = "rate_limit", now = Date.now() }) {
    return this.call(1, "setLock", { provider, connectionId, model, untilMs, status, reason, now });
  }

  clearLock({ provider, connectionId, model, expectedGeneration }) {
    return this.call(1, "clearLock", { provider, connectionId, model, expectedGeneration });
  }

  getActiveLocks({ provider, connectionIds, model, now = Date.now() }) {
    return this.call(1, "getActiveLocks", { provider, connectionIds, model, now });
  }

  recordOutcome({ provider, connectionId, model, success, latencyMs, status, costUsd = 0, now = Date.now() }) {
    return this.call(1, "recordOutcome", { provider, connectionId, model, success, latencyMs, status, costUsd, now });
  }

  recordOutcomes({ items, now = Date.now() }) {
    return this.call(1, "recordOutcomes", { items, now });
  }

  getScores({ provider, connectionIds, model, now = Date.now() }) {
    return this.call(1, "getScores", { provider, connectionIds, model, now });
  }

  reserveBudget({ keyId, requestId, amount, expiresAt, availableBudget, now = Date.now() }) {
    return this.call(1, "reserveBudget", { keyId, requestId, amount, expiresAt, availableBudget, now });
  }

  releaseBudget({ keyId, requestId }) {
    return this.call(1, "releaseBudget", { keyId, requestId });
  }

  bumpVersion({ domain }) {
    return this.call(1, "bumpVersion", { domain });
  }

  getVersions({ domains }) {
    return this.call(1, "getVersions", { domains });
  }

  // --- CoordinatorCoalescer Methods ---

  readCached(key, now = Date.now()) {
    return this.call(2, "readCached", { key, now });
  }

  setCached(key, scope, value, now = Date.now(), ttlMs) {
    return this.call(2, "setCached", { key, scope, value, now, ttlMs });
  }

  invalidateCache(scope) {
    return this.call(2, "invalidate", { scope });
  }

  queueOutcome({ provider, connectionId, model, success, latencyMs, status = 200, costUsd = 0, now = Date.now() }) {
    return this.call(2, "queueOutcome", { provider, connectionId, model, success, latencyMs, status, costUsd, now });
  }

  queueMetrics({ tenant, provider, connectionId, model, success, status = 200, latencyMs, now = Date.now() }) {
    return this.call(2, "queueMetrics", { tenant, provider, connectionId, model, success, status, latencyMs, now });
  }

  flushCoalesced(now = Date.now()) {
    return this.call(2, "flush", { now });
  }

  getPendingCounts() {
    return this.call(2, "pendingCounts", {});
  }

  // --- Fast Path Route Resolver Methods ---

  configureRouting({ apiKeys = [], aliases = {}, combos = [], connections = [] }) {
    return this.call(3, "configure", { apiKeys, aliases, combos, connections });
  }

  resolveRoute({ model, apiKey, path = "/v1/chat/completions", now = Date.now() }) {
    return this.call(3, "resolveRoute", { model, apiKey, path, now });
  }

  // --- Zero-Allocation High-Speed Scalar Fast-Path Methods ---

  fastCheckRateLimit(keyId, limit, windowMs, now = Date.now()) {
    const keyAlloc = this._allocString(keyId);
    try {
      const nowBig = BigInt(now);
      const nowLow = Number(nowBig & 0xFFFFFFFFn);
      const nowHigh = Number(nowBig >> 32n);
      const packed = BigInt(this.exports.edge_fast_check_rate_limit(
        keyAlloc.ptr,
        keyAlloc.len,
        limit,
        windowMs,
        nowLow,
        nowHigh
      ));
      const allowed = (packed & 0x8000000000000000n) !== 0n;
      const resetDelta = Number((packed >> 32n) & 0x7FFFFFFFn);
      const remaining = Number(packed & 0x7FFFFFFFn);
      return { allowed, remaining, resetAt: now + resetDelta };
    } finally {
      if (keyAlloc.ptr) this.exports.wasm_dealloc(keyAlloc.ptr, keyAlloc.len);
    }
  }

  fastBeginRequest(keyId, maxConcurrent, now = Date.now()) {
    const keyAlloc = this._allocString(keyId);
    try {
      const nowBig = BigInt(now);
      const nowLow = Number(nowBig & 0xFFFFFFFFn);
      const nowHigh = Number(nowBig >> 32n);
      const packed = BigInt(this.exports.edge_fast_begin_request(
        keyAlloc.ptr,
        keyAlloc.len,
        maxConcurrent,
        nowLow,
        nowHigh
      ));
      const allowed = (packed & 0x8000000000000000n) !== 0n;
      const active = Number(packed & 0x7FFFFFFFn);
      let leaseId = null;
      if (allowed) {
        const ptr = this.exports.edge_wasm_get_result_ptr();
        const len = this.exports.edge_wasm_get_result_len();
        if (ptr && len) {
          leaseId = this.textDecoder.decode(new Uint8Array(this.memory.buffer, ptr, len));
        }
      }
      return { allowed, active, leaseId };
    } finally {
      if (keyAlloc.ptr) this.exports.wasm_dealloc(keyAlloc.ptr, keyAlloc.len);
    }
  }

  fastEndRequest(keyId, leaseId) {
    const keyAlloc = this._allocString(keyId);
    const leaseAlloc = this._allocString(leaseId);
    try {
      return this.exports.edge_fast_end_request(
        keyAlloc.ptr,
        keyAlloc.len,
        leaseAlloc.ptr,
        leaseAlloc.len
      );
    } finally {
      if (keyAlloc.ptr) this.exports.wasm_dealloc(keyAlloc.ptr, keyAlloc.len);
      if (leaseAlloc.ptr) this.exports.wasm_dealloc(leaseAlloc.ptr, leaseAlloc.len);
    }
  }

  fastClearLock(provider, connectionId, model = "", expectedGeneration = -1) {
    const provAlloc = this._allocString(provider);
    const connAlloc = this._allocString(connectionId);
    const modelAlloc = this._allocString(model || "");
    try {
      const exp = expectedGeneration == null ? -1n : BigInt(expectedGeneration);
      return this.exports.edge_fast_clear_lock(
        provAlloc.ptr,
        provAlloc.len,
        connAlloc.ptr,
        connAlloc.len,
        modelAlloc.ptr,
        modelAlloc.len,
        exp
      ) === 1;
    } finally {
      if (provAlloc.ptr) this.exports.wasm_dealloc(provAlloc.ptr, provAlloc.len);
      if (connAlloc.ptr) this.exports.wasm_dealloc(connAlloc.ptr, connAlloc.len);
      if (modelAlloc.ptr) this.exports.wasm_dealloc(modelAlloc.ptr, modelAlloc.len);
    }
  }
}
