import { fileURLToPath } from "node:url";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const projectRoot = dirname(fileURLToPath(import.meta.url));
const isCloudflareBuild = process.env.NINEROUTER_RUNTIME === "cloudflare";
// CLI bundling needs workspace root so tracing includes hoisted node_modules (slim ~50MB).
// Docker / default uses projectRoot so server.js lands at /app/server.js (not nested).
const tracingRoot = process.env.NEXT_TRACING_ROOT_MODE === "workspace"
  ? join(projectRoot, "..")
  : projectRoot;
// Next clones and buffers every request body that passes the proxy/middleware, up to this
// size, and silently truncates past it. 128mb equals a Worker isolate's whole memory, so the
// Cloudflare build buffers at most the largest accepted upload (32 MB, see
// open-sse/config/bodyLimits.js; the worker guard answers 413 above the limits first).
const proxyClientMaxBodySize = process.env.NINEROUTER_PROXY_CLIENT_MAX_BODY_SIZE || (isCloudflareBuild ? "32mb" : "128mb");

/** @type {import('next').NextConfig} */
const nextConfig = {
  distDir: process.env.NEXT_DIST_DIR || ".next",
  output: "standalone",
  generateBuildId: async () => {
    return process.env.NEXT_BUILD_ID || "9router-production-build";
  },
  // `open` must stay external. It derives its own directory from `import.meta.url`, and
  // webpack replaces that with the absolute path of the BUILD machine as a string literal.
  // A release built on macOS therefore ships `file:///Users/.../open/index.js`, which
  // `fileURLToPath` rejects on Windows ("File URL path must be absolute" — no drive
  // letter). That throw happens at module scope, so every consumer of `open` dies on
  // import — including xAI/Grok token refresh, which loads the OAuth service that imports
  // it. Keeping it external preserves the real `import.meta.url` at runtime.
  // @aws-sdk/credential-providers must stay external: it reads ~/.aws/config
  // and the SSO token cache from disk at runtime and resolves credential
  // plugins by dynamic require, neither of which survives bundling.
  // wreq-js resolves its rust native binding by relative require paths from its own
  // package dir — bundling breaks those paths ("Failed to load native module").
  // undici must stay external too: webpack's vendored copy drops the per-request
  // `dispatcher` connect options (ecdhCurve) — the built server then RSTs on
  // DPI-shaped networks while the dev server (real undici) passes.
  serverExternalPackages: ["better-sqlite3", "sql.js", "node:sqlite", "bun:sqlite", "open", "@aws-sdk/credential-providers", "wreq-js", "undici"].filter(() => !isCloudflareBuild),
  turbopack: {
    root: tracingRoot
  },
  outputFileTracingRoot: tracingRoot,
  outputFileTracingIncludes: {
    // serverExternalPackages keeps undici external; dynamic import("undici")
    // alone doesn't pull it into the trace, so force-ship the real package.
    "*": ["./node_modules/undici/**/*"]
  },
  outputFileTracingExcludes: {
    "*": ["./gitbook/**/*"]
  },
  images: {
    unoptimized: true
  },
  env: {},
  experimental: {
    // #1529/#1572: LLM clients can send long context or base64 image payloads through /v1 rewrites.
    proxyClientMaxBodySize,
    // Cache fetch responses across HMR refreshes for faster dev reloads.
    serverComponentsHmrCache: true,
    // Tree-shake heavy barrel imports to cut compile + bundle size
    optimizePackageImports: ["@xyflow/react", "@dnd-kit/core", "@dnd-kit/sortable", "material-symbols", "marked"],
  },
  webpack: (config, { isServer }) => {
    // Ignore fs/path modules in browser bundle
    if (!isServer) {
      config.resolve.fallback = {
        ...config.resolve.fallback,
        fs: false,
        path: false,
      };
    }
    // Bundle analysis: NINEROUTER_WEBPACK_STATS=/path/stats.json writes a per-module size report
    // of the server compilation (docs/CLOUDFLARE_WORKER_SPLIT.md explains how it is used).
    if (process.env.NINEROUTER_WEBPACK_STATS && isServer) {
      config.plugins.push({
        apply(compiler) {
          compiler.hooks.done.tap("NineRouterStats", (stats) => {
            const json = stats.toJson({ all: false, modules: true, nestedModules: true, reasons: false, chunks: false, assets: false });
            const rows = [];
            const walk = (mods) => {
              for (const m of mods || []) {
                if (m.modules && m.modules.length) walk(m.modules);
                else rows.push({ name: m.name, size: m.size });
              }
            };
            walk(json.modules);
            writeFileSync(process.env.NINEROUTER_WEBPACK_STATS + "." + (compiler.name || "server"), JSON.stringify(rows));
          });
        },
      });
    }
    // Cloudflare Worker bundle diet (Free plan: 3 MB compressed, Paid: 10 MB). These packages are
    // dead weight on Workers: native-only, filesystem/CLI-only, or replaced by the runtime's
    // fetch. Each is aliased to a stub that throws only if something really touches it.
    if (isCloudflareBuild && isServer) {
      const stub = (name) => join(projectRoot, "src", "cloudflare", "stubs", name);
      config.resolve.alias = {
        ...config.resolve.alias,
        "node-forge$": stub("node-forge.cjs"),
        "@node-saml/node-saml$": stub("node-saml.cjs"),
        "@aws-sdk/credential-providers$": stub("aws-credential-providers.cjs"),
        undici$: stub("undici.cjs"),
        "socks-proxy-agent$": stub("socks-proxy-agent.cjs"),
        // Client-only: reached solely through next/dynamic({ ssr: false }), which never runs on the
        // server, but webpack still compiles the lazy chunk into the server bundle.
        recharts$: stub("client-only.cjs"),
        "@xyflow/react$": stub("client-only.cjs"),
      };
    }
    // Exclude non-source dirs from watcher to reduce inotify load
    config.watchOptions = {
      ...config.watchOptions,
      aggregateTimeout: 300,
      ignored: /[\\/](node_modules|\.git|logs|\.next|\.next-cli-build|gitbook|cli|open-sse\.old|tests|docs)[\\/]/,
    };
    return config;
  },
  async rewrites() {
    return [
      {
        source: "/v1/v1/:path*",
        destination: "/api/v1/:path*"
      },
      {
        source: "/v1/v1",
        destination: "/api/v1"
      },
      {
        source: "/codex/:path*",
        destination: "/api/v1/responses"
      },
      {
        source: "/responses",
        destination: "/api/v1/responses"
      },
      {
        source: "/systemone",
        destination: "/api/v1/systemone"
      },
      {
        source: "/v1beta/:path*",
        destination: "/api/v1beta/:path*"
      },
      {
        source: "/v1beta",
        destination: "/api/v1beta"
      },
      {
        source: "/v1/:path*",
        destination: "/api/v1/:path*"
      },
      {
        source: "/v1",
        destination: "/api/v1"
      }
    ];
  }
};

export default nextConfig;
