// Config for the live-provider suites under tests/e2e/real.
//
// These are the only tests that share the user's real ~/.9router database and
// their real provider credentials. Two things make the default config wrong for
// them:
//
//   1. fileParallelism — every real suite writes to the same SQLite file (token
//      refresh, connection rotation, usage rows). Vitest runs test files in
//      parallel and SQLite has a single writer, so concurrent files failed with
//      "database is locked" — a harness artefact, not a product defect.
//   2. RUN_REAL is still required; this config does not enable live calls on its
//      own, so a stray `vitest run` stays offline.
//
// Usage:
//   RUN_REAL=1 npx vitest run -c vitest.real.config.js e2e/real/
import { defineConfig } from "vitest/config";
import { resolve } from "path";
import { fileURLToPath } from "url";

const __dirname = fileURLToPath(new URL(".", import.meta.url));

export default defineConfig({
  test: {
    environment: "node",
    globals: true,
    include: ["e2e/real/**/*.test.js"],
    fileParallelism: false,
    testTimeout: 120000,
    hookTimeout: 120000,
  },
  resolve: {
    alias: [
      { find: /^open-sse\//, replacement: resolve(__dirname, "../open-sse") + "/" },
      { find: "open-sse", replacement: resolve(__dirname, "../open-sse") },
      { find: /^@\//, replacement: resolve(__dirname, "../src") + "/" },
    ],
  },
});
