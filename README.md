# 9Router (`9r`)

**High-Throughput Native AI Gateway, Lossless RTK Token Saver & Zero-Dependency CLI Suite**

Connect all AI coding tools (Claude Code, Cursor, Codex, OpenCode, Cline, Pi, Oh My Pi, Crush, Factory Droid) to 40+ AI providers and 100+ models with automatic failover, hedged racing, exact-match token caching, and built-in daemon management.

---

## ⚡ One-Line Install

Install the native binary (`9r`) with zero runtime dependencies (no Node.js, npm, or Python required):

```bash
curl -fsSL https://raw.githubusercontent.com/mihir0209/9router/master/install | bash
```

The installer automatically detects your operating system (Linux, macOS, Windows) and architecture (x64, arm64), places the binary in `~/.local/bin/9r`, creates a `9router` compatibility alias, and updates your shell profile `PATH`.

---

## 🚀 Quick Start

### 1. Start the Gateway Daemon
```bash
9r start -d
```

### 2. Check Gateway Status
```bash
9r status
```
```
✓ 9Router is running
   PID:        226735
   Port:       20127
   Endpoint:   http://127.0.0.1:20127
   Memory:     9.1 MB RSS
   Socket:     Active (Listening)
   Log File:   ~/.9router/run/gateway.log
```

### 3. Connect Your Coding Tools
Auto-configure your local AI coding agents to route through 9Router:
```bash
9r connect http://localhost:20127
```

Verify your active tool configurations:
```bash
9r show
```

### 4. Stop the Daemon
```bash
9r stop
```

---

## 📊 Performance & Efficiency Benchmarks

| Metric | Legacy Node.js Core | Native Rust `9r` Core | Advantage |
| :--- | :--- | :--- | :--- |
| **Cold Boot Time** | ~2,400 ms | **< 1 ms** | **3,000x faster** |
| **Idle Memory (RSS)** | ~148 MB | **8.5 MB** | **17x lower footprint** |
| **Cache Replay Latency** | ~28 ms | **0.35 ms** | **80x faster** |
| **Runtime Dependencies** | Node.js, npm, Python | **0 (Zero)** | **Self-contained single binary** |
| **Disk Footprint** | 840 MB (`node_modules`) | **11 MB binary** | **76x smaller** |
| **Container Image Size** | ~480 MB | **26 MB (Alpine musl)** | **18x smaller** |

---

## 💡 Key Architectural Capabilities

### 1. In-Memory RTK Token Saver
Saves **20% to 40%** tokens per request by automatically and losslessly compressing bulky tool output blocks (`tool_result` and `tool` messages) before forwarding to upstream providers:
- **`git diff`**: Strips redundant unified diff headers and truncates hunk padding beyond 100 lines while preserving semantic code changes.
- **`grep`**: Groups repeated file path prefixes into structured per-file match summaries.
- **`ls` / `find`**: Collapses directory listings and redundant tabular formatting.
- **`build_output`**: Deduplicates repetitive compiler warnings and progress bars while preserving critical error messages untouched.
- **Safety Guarantee**: In-memory inspection guarantees that compressed output is never empty and never expands the original text byte length. Error blocks (`is_error: true`) are preserved untouched.

### 2. Multi-Tier Security & Authentication
- **Public Client Endpoints** (`/v1/chat/completions`, `/v1/messages`): Protected by provisioned API keys (`Bearer sk-...` or `x-api-key`).
- **Web Dashboard Endpoints** (`/api/dashboard/*`, `/api/connections`, `/api/combos`): Protected by cryptographically signed JWT session cookies with timing-safe SHA-256 password verification.
- **Automated Management Endpoints** (`/api/admin/*`): Protected by high-entropy Admin API tokens (`x-admin-token`).
- **File System Permissions**: Sensitive configuration files and backups are strictly locked down to `0600` (user read/write only) on POSIX platforms.

### 3. Speculative Hedged Combos & Circuit Breakers
- **Hedged Racing**: Races multiple provider endpoints with configurable stagger delays (`hedgedDelayMs`), cancelling pending speculative branches the moment the fastest provider produces its first token.
- **Automatic Fallback**: Transparently cascades across provider tiers (Subscription → Cheap → Free) when rate limits or HTTP 429/5xx errors occur.
- **Sliding-Window Circuit Breakers**: Automatically isolates failing upstream connections and periodically dispatches canary requests to safely verify recovery.

### 4. Exact-Match LRU Token Cache
- Caches identical chat completions and message payloads in-memory.
- Serves cache hits in sub-millisecond latency (**0.35 ms**) with streaming SSE replay, eliminating duplicate token costs entirely.

---

## 🛠️ Complete CLI Command Reference

```
9Router (9r) - Single-Binary AI Gateway, Token Saver & CLI Daemon Manager

Usage:
  9r [command] [options]

Core Commands:
  run                      Run 9Router AI gateway in foreground (default)
  start [-d] [--port <p>]  Start gateway in background daemon mode (-d) or foreground
  status                   Show running gateway daemon status (PID, port, memory RSS, logs)
  stop                     Stop the background gateway daemon
  restart [-d]             Restart the gateway daemon
  logs [-n <lines>] [-f]   View gateway logs (with optional follow mode)

Tool Configuration:
  connect [url] [options]  Auto-configure local AI coding tools (Claude, Codex, OpenCode, etc.)
  show [tools] [--json]    Display current 9Router configuration across local coding tools

OS Service Integration:
  service install          Install 9Router as native user autostart service (systemd / launchd)
  service uninstall        Remove native user autostart service
  service status           Inspect native user service status

Flags:
  -v, --version            Display version information
  -h, --help               Display help text
```

### Supported Coding Tools for `9r connect`
- **Claude Code**: `~/.claude/settings.json` (supports `fable`, `opus`, `sonnet`, and `haiku` model tiers)
- **OpenAI Codex CLI**: `~/.codex/config.toml`
- **OpenCode**: `~/.config/opencode/opencode.json`
- **Cursor & Cline**: `~/.cursorrules`, `~/.cline/data/globalState.json`
- **Factory Droid**: `~/.factory/settings.json`
- **Crush**: `~/.config/crush/crush.json`
- **Kilo Code CLI**: `~/.local/share/kilo/auth.json`
- **Pi & Oh My Pi**: `~/.pi/agent/models.json`, `~/.omp/agent/models.yml`

Automated backup files (`*.bak-9router`) are generated prior to modifying any tool configuration.

---

## 📦 Container Deployment

Run the static 26MB container image on any container runtime:

```bash
docker run -d \
  -p 20127:20127 \
  -v ~/.9router:/root/.9router \
  --name 9router \
  mihir0209/9router:latest
```

---

## 📄 License

MIT License. Crafted with precision for high-throughput AI engineering.
