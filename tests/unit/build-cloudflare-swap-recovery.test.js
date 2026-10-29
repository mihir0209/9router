import { describe, it, expect, beforeEach, afterEach } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { recoverStaleSwaps } from "../../scripts/lib/swap-recovery.mjs";

let root;
const w = (rel, text) => {
  const file = path.join(root, rel);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
};
const r = (rel) => fs.readFileSync(path.join(root, rel), "utf8");
const has = (rel) => fs.existsSync(path.join(root, rel));

beforeEach(() => { root = fs.mkdtempSync(path.join(os.tmpdir(), "swap-recovery-")); });
afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

describe("recoverStaleSwaps", () => {
  it("does nothing on a clean tree", () => {
    w("src/proxy.js", "node-proxy");
    w("src/proxy.cloudflare.js", "cf-proxy");
    w("src/lib/db/driver.js", "node-driver");
    w("src/lib/db/driver.cloudflare.js", "cf-driver");
    expect(recoverStaleSwaps(root)).toEqual([]);
    expect(r("src/proxy.js")).toBe("node-proxy");
    expect(r("src/lib/db/driver.js")).toBe("node-driver");
  });

  it("restores a fully applied swap left by a killed build", () => {
    w("src/proxy.js.cloudflare-disabled", "node-proxy");
    w("src/middleware.js", "cf-proxy");
    w("src/lib/db/driver.js.cloudflare-disabled", "node-driver");
    w("src/lib/db/driver.js", "cf-driver");
    expect(recoverStaleSwaps(root).sort()).toEqual(["driver", "proxy"]);
    expect(r("src/proxy.js")).toBe("node-proxy");
    expect(r("src/proxy.cloudflare.js")).toBe("cf-proxy");
    expect(r("src/lib/db/driver.js")).toBe("node-driver");
    expect(r("src/lib/db/driver.cloudflare.js")).toBe("cf-driver");
    expect(has("src/middleware.js")).toBe(false);
    expect(has("src/proxy.js.cloudflare-disabled")).toBe(false);
    expect(has("src/lib/db/driver.js.cloudflare-disabled")).toBe(false);
  });

  it("restores a swap interrupted between the two renames", () => {
    w("src/proxy.js.cloudflare-disabled", "node-proxy");
    w("src/proxy.cloudflare.js", "cf-proxy");
    expect(recoverStaleSwaps(root)).toEqual(["proxy"]);
    expect(r("src/proxy.js")).toBe("node-proxy");
    expect(r("src/proxy.cloudflare.js")).toBe("cf-proxy");
  });

  it("refuses an ambiguous state instead of overwriting real work", () => {
    w("src/proxy.js.cloudflare-disabled", "parked");
    w("src/proxy.js", "something else");
    expect(() => recoverStaleSwaps(root)).toThrow(/both exist/);
    expect(r("src/proxy.js")).toBe("something else");
  });

  it("is idempotent", () => {
    w("src/proxy.js.cloudflare-disabled", "node-proxy");
    w("src/middleware.js", "cf-proxy");
    recoverStaleSwaps(root);
    expect(recoverStaleSwaps(root)).toEqual([]);
  });
});
