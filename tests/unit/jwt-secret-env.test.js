import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

vi.mock("@/lib/localDb", () => ({
  getSettings: vi.fn(async () => ({})),
}));

/**
 * The dashboard session token is signed in the Node runtime and verified in the
 * Edge runtime (the proxy/middleware). The Edge sandbox has no filesystem, so
 * it can only read process.env — if the secret stays in <dataDir>/jwt-secret
 * the two sides use different keys and every authenticated request is bounced
 * back to /login. ensureJwtSecretEnv() publishes the file's secret into the
 * environment so both sides agree.
 */
describe("ensureJwtSecretEnv", () => {
  let tempDir;
  const originalSecret = process.env.JWT_SECRET;
  const originalDataDir = process.env.DATA_DIR;

  beforeEach(() => {
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-jwt-"));
    process.env.DATA_DIR = tempDir;
    delete process.env.JWT_SECRET;
  });

  afterEach(() => {
    if (originalSecret === undefined) delete process.env.JWT_SECRET;
    else process.env.JWT_SECRET = originalSecret;
    if (originalDataDir === undefined) delete process.env.DATA_DIR;
    else process.env.DATA_DIR = originalDataDir;
    fs.rmSync(tempDir, { recursive: true, force: true });
  });

  it("keeps an already-set secret untouched", async () => {
    process.env.JWT_SECRET = "from-the-environment";
    const { ensureJwtSecretEnv } = await import("@/lib/auth/jwtSecret.js");
    expect(ensureJwtSecretEnv()).toBe("from-the-environment");
    expect(process.env.JWT_SECRET).toBe("from-the-environment");
  });

  it("publishes the data-dir file's secret into the environment", async () => {
    fs.mkdirSync(path.join(tempDir, "db"), { recursive: true });
    fs.writeFileSync(path.join(tempDir, "jwt-secret"), "file-secret-value\n");
    const { ensureJwtSecretEnv } = await import("@/lib/auth/jwtSecret.js");
    expect(ensureJwtSecretEnv()).toBe("file-secret-value");
    expect(process.env.JWT_SECRET).toBe("file-secret-value");
  });

  it("creates the secret file when it does not exist yet", async () => {
    const { ensureJwtSecretEnv } = await import("@/lib/auth/jwtSecret.js");
    const secret = ensureJwtSecretEnv();
    expect(secret).toBeTruthy();
    expect(process.env.JWT_SECRET).toBe(secret);
    const onDisk = fs.readFileSync(path.join(tempDir, "jwt-secret"), "utf8").trim();
    expect(onDisk).toBe(secret);
  });

  it("is idempotent across repeated calls", async () => {
    const { ensureJwtSecretEnv } = await import("@/lib/auth/jwtSecret.js");
    const first = ensureJwtSecretEnv();
    const second = ensureJwtSecretEnv();
    expect(second).toBe(first);
  });

  it("makes a signed token verifiable by the same secret the Edge side reads", async () => {
    // The Node runtime signs; the Edge runtime verifies. Both must land on the
    // same key — this is the exact pair that broke the dashboard before.
    fs.writeFileSync(path.join(tempDir, "jwt-secret"), "shared-secret-for-both-runtimes");
    const { ensureJwtSecretEnv } = await import("@/lib/auth/jwtSecret.js");
    ensureJwtSecretEnv();

    const { createDashboardAuthToken, verifyDashboardAuthToken } = await import("@/lib/auth/dashboardSession.js");
    const token = await createDashboardAuthToken();
    expect(await verifyDashboardAuthToken(token)).toBe(true);
  });
});
