const http = require("http");
const path = require("path");
const fs = require("fs");
const crypto = require("crypto");
const { pathToFileURL } = require("url");

const origCreate = http.createServer.bind(http);

// Propagate an explicit --port/-p CLI argument into process.env.PORT before the
// server boots, so every runtime `process.env.PORT || "20128"` derivation (docs
// URLs, tunnels, OAuth redirects, model tests) matches the port the server
// actually listens on. The env var wins when already set (the CLI launcher
// always sets PORT for its child process).
(function propagatePortArg() {
  if (process.env.PORT) return;
  const argv = process.argv.slice(2);
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--port" || argv[i] === "-p") {
      const value = parseInt(argv[i + 1], 10);
      if (Number.isInteger(value) && value > 0 && value <= 65535) {
        process.env.PORT = String(value);
      }
      break;
    }
  }
})();

// Per-process secret proving x-9r-real-ip was stamped below rather than sent by the client.
// A bare `next start` / `next dev` never loads this file, so it cannot produce a matching
// header even though the env var is inherited by child processes. Named like x-9r-cli-token
// so the request-detail header sanitizer redacts it too.
const PEER_TOKEN = crypto.randomBytes(24).toString("hex");
process.env.NINEROUTER_PEER_TOKEN = PEER_TOKEN;

// Publish the dashboard JWT secret into the environment before the server
// boots. The proxy runs in the Edge runtime and has no filesystem, so it can
// only verify the session token against process.env; without this it uses a
// different key than the route handler that signed it and every dashboard
// request is bounced back to /login.
function publishJwtSecret() {
  if (process.env.JWT_SECRET) return;
  try {
    const fs = require("node:fs");
    const path = require("node:path");
    const os = require("node:os");
    const dataDir = process.env.DATA_DIR || path.join(os.homedir(), ".9router");
    const file = path.join(dataDir, "jwt-secret");
    let secret = "";
    try {
      secret = fs.readFileSync(file, "utf8").trim();
    } catch {
      fs.mkdirSync(dataDir, { recursive: true });
      secret = crypto.randomBytes(32).toString("hex");
      fs.writeFileSync(file, secret, { mode: 0o600 });
    }
    if (secret) process.env.JWT_SECRET = secret;
  } catch {
    /* best effort — the app bootstraps the same secret on its own */
  }
}

publishJwtSecret();

let backgroundRefreshStarted = false;

function startBackgroundTokenRefreshFromCustomServer() {
  if (backgroundRefreshStarted) return;
  backgroundRefreshStarted = true;
  // Prefer source path (repo / standalone that still has src). Fail-open if missing
  // — initializeApp also starts the same scheduler when the Next app boots.
  const modPath = path.join(__dirname, "src", "sse", "services", "backgroundTokenRefresh.mjs");
  import(pathToFileURL(modPath).href)
    .then((m) => {
      try {
        m.startBackgroundTokenRefresh();
      } catch (e) {
        console.error("[BackgroundTokenRefresh] start failed:", e && e.message ? e.message : e);
      }
      const stop = () => {
        try {
          m.stopBackgroundTokenRefresh();
        } catch {
          /* ignore */
        }
      };
      process.once("SIGINT", stop);
      process.once("SIGTERM", stop);
    })
    .catch((e) => {
      // Expected in published CLI standalone (src/ not on disk). App bootstrap covers it.
      if (process.env.DEBUG_BACKGROUND_TOKEN_REFRESH) {
        console.error("[BackgroundTokenRefresh] import failed:", e && e.message ? e.message : e);
      }
    });
}

// Wrap Next standalone HTTP server: derive client IP from the TCP socket
// (unspoofable) and strip client-supplied forwarding headers so downstream
// rate-limiting keys on the real peer address instead of attacker-controlled XFF.
http.createServer = (...args) => {
  const handler = args.find((a) => typeof a === "function");
  const rest = args.filter((a) => typeof a !== "function");
  if (!handler) return origCreate(...args);
  const wrapped = (req, res) => {
    const socketIp = req.socket && req.socket.remoteAddress ? req.socket.remoteAddress : "";
    const xff = req.headers["x-forwarded-for"];
    const xRealIp = req.headers["x-real-ip"];
    const viaProxy = !!(xff || xRealIp);
    const isLoopbackProxy = socketIp === "127.0.0.1" || socketIp === "::1" || socketIp === "::ffff:127.0.0.1";
    // Trust forwarding headers only when the TCP peer is a local reverse proxy.
    // Direct/public sockets remain keyed by the unspoofable peer address.
    const proxyIp = xRealIp || (xff ? String(xff).split(",")[0].trim() : "");
    const ip = isLoopbackProxy && proxyIp ? proxyIp : socketIp;
    delete req.headers["x-9r-real-ip"];
    delete req.headers["x-forwarded-for"];
    delete req.headers["x-9r-via-proxy"];
    delete req.headers["x-9r-peer-token"];
    req.headers["x-9r-real-ip"] = ip;
    req.headers["x-9r-peer-token"] = PEER_TOKEN;
    if (viaProxy) req.headers["x-9r-via-proxy"] = "1";
    return handler(req, res);
  };
  const server = origCreate(...rest, wrapped);
  server.once("listening", () => {
    startBackgroundTokenRefreshFromCustomServer();
  });
  const origEmit = server.emit;
  // JBR 25 sends h2c upgrades that the HTTP/1.1 server would otherwise close.
  server.emit = function (event, ...eventArgs) {
    const [req, socket, head] = eventArgs;
    if (event !== "upgrade" || String(req.headers.upgrade || "").toLowerCase() !== "h2c") {
      return origEmit.call(this, event, ...eventArgs);
    }

    const contentLength = Number(req.headers["content-length"] || 0);
    if (!Number.isSafeInteger(contentLength) || contentLength < 0) {
      socket.destroy();
      return true;
    }
    const chunks = [head];
    let received = head.length;
    const serve = () => {
      // Replay the upgraded request through the existing HTTP/1.1 handler.
      const replay = new http.IncomingMessage(socket);
      Object.assign(replay, { method: req.method, url: req.url, headers: req.headers, complete: true });
      if (received) replay.push(Buffer.concat(chunks, received).subarray(0, contentLength));
      replay.push(null);
      const res = new http.ServerResponse(replay);
      res.shouldKeepAlive = false;
      res.assignSocket(socket);
      res.once("finish", () => socket.end());
      Promise.resolve().then(() => wrapped(replay, res)).catch((error) => {
        console.error("Failed to downgrade h2c request", error);
        socket.destroy();
      });
    };
    if (received >= contentLength) serve();
    else {
      socket.on("data", function readBody(chunk) {
        chunks.push(chunk);
        received += chunk.length;
        if (received < contentLength) return;
        socket.off("data", readBody);
        serve();
      });
      socket.resume();
    }
    delete req.headers.upgrade;
    delete req.headers["http2-settings"];
    req.headers.connection = "close";
    return true;
  };
  return server;
};

if (require.main === module) {
  // Refuse the Cloudflare worker build variant. Both builds write a
  // standalone directory; the Cloudflare one (scripts/build-cloudflare.mjs)
  // compiles in the D1-backed driver, so booted locally it serves an
  // ephemeral in-memory database: the local SQLite file is ignored and
  // every import or edit vanishes on restart. Failing here beats silently
  // running a server that appears to lose data.
  const assertNotCloudflareBuild = (serverDir) => {
    try {
      const variant = fs.readFileSync(path.join(serverDir, ".9router-build-variant"), "utf8").trim();
      if (variant === "cloudflare" && process.env.NINEROUTER_ALLOW_CLOUDFLARE_STANDALONE !== "1") {
        console.error([
          "[custom-server] This standalone build is the CLOUDFLARE WORKER variant (from `npm run build:cloudflare`).",
          "Booted locally it uses an ephemeral in-memory database and ignores the local SQLite data:",
          "imports and edits disappear on restart.",
          "",
          "Run `npm run build` first to produce the Node standalone variant, then `npm start`.",
          "(Set NINEROUTER_ALLOW_CLOUDFLARE_STANDALONE=1 to override, at your own risk.)",
        ].join("\n"));
        process.exit(1);
      }
    } catch {
      /* No marker (older build) — assume the Node variant and boot. */
    }
  };

  const standalone = path.join(__dirname, "server.js");
  if (fs.existsSync(standalone)) {
    // Running from inside a standalone directory (the wrapper is copied next
    // to server.js). The marker, if any, sits beside it.
    assertNotCloudflareBuild(__dirname);
    require(standalone);
  } else {
    // A standalone build lives under .next/standalone (next.config sets
    // output: "standalone"). `next start` refuses to run against that output
    // and exits, so prefer the built standalone server when it exists.
    const standaloneBuild = path.join(__dirname, ".next", "standalone", "server.js");
    if (fs.existsSync(standaloneBuild)) {
      assertNotCloudflareBuild(path.dirname(standaloneBuild));
      // Next's standalone server chdirs into its own directory and reads
      // process.env.PORT, which the --port handling above already set.
      require(standaloneBuild);
    } else {
      // No standalone build next to us (e.g. a source-only checkout).
      // `next start` builds its HTTP server in-process, so the wrapper above
      // still sanitizes every request.
      const nextBin = require.resolve("next/dist/bin/next");
      process.argv = [process.argv[0], nextBin, "start", ...process.argv.slice(2)];
      require(nextBin);
    }
  }
}
