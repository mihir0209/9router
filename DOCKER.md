# Docker

Run 9Router in a container. Published image: [`mihir0209/9router`](https://hub.docker.com/r/mihir0209/9router) (zero-dependency native Rust single binary).

---

# 👤 For Users

## Quick start

```bash
docker run -d \
  -p 20127:20127 \
  -v "$HOME/.9router:/app/data" \
  -e DATA_DIR=/app/data \
  --name 9router \
  mihir0209/9router:latest
```

App listens on port `20127` by default (override via `-e PORT=20128` if needed). Open: http://localhost:20127

## Manage container

```bash
docker logs -f 9router        # view logs
docker stop 9router           # stop
docker start 9router          # start again
docker rm -f 9router          # remove
```

## Data persistence

```bash
-v "$HOME/.9router:/app/data" \
-e DATA_DIR=/app/data
```

Without `DATA_DIR`, the app falls back to `~/.9router/` (macOS/Linux) or `%APPDATA%\9router\` (Windows). In the container, `DATA_DIR=/app/data` makes the bind mount work.

Data layout under `$DATA_DIR/`:

```text
$DATA_DIR/
├── db/
│   ├── data.sqlite       # main SQLite database
│   └── backups/          # auto backups
└── ...                   # certs, logs, runtime configs
```

Host path: `$HOME/.9router/db/data.sqlite`
Container path: `/app/data/db/data.sqlite`

## Optional env vars

```bash
docker run -d \
  -p 20128:20128 \
  -v "$HOME/.9router:/app/data" \
  -e DATA_DIR=/app/data \
  -e PORT=20128 \
  -e HOST=0.0.0.0 \
  -e STANDALONE=1 \
  --name 9router \
  mihir0209/9router:latest
```

## Optional Headroom sidecar

The 9Router container embeds native lossless RTK token saving directly in memory. To run with an external Headroom sidecar proxy:

```yaml
services:
  9router:
    image: mihir0209/9router:latest
    ports:
      - "20128:20128"
    volumes:
      - "$HOME/.9router:/app/data"
    environment:
      DATA_DIR: /app/data
      PORT: "20128"
      HOST: "0.0.0.0"
      HEADROOM_URL: http://headroom:8787
    depends_on:
      - headroom

  headroom:
    image: ghcr.io/chopratejas/headroom:latest
    ports:
      - "8787:8787"
```

## Docker Compose Profiles

`docker-compose.yml` provides two production service profiles:

### 1. SQLite Profile (Single-Node High Performance)
Runs single-node 9Router with lock-free WAL journal mode and persistent volume mount:

```bash
docker compose --profile sqlite up -d
```

### 2. Postgres Profile (Clustered Multi-Node)
Runs clustered 9Router connected to a healthy PostgreSQL 16 Alpine database with connection pooling and schema isolation:

```bash
docker compose --profile postgres up -d
```

---

# ☸ Kubernetes Production Deployment

Full Kubernetes manifests are available in [`deploy/k8s/`](file:///home/ghostrider/.gemini/antigravity/worktrees/d6bd882f-9161-40f7-b8fb-24810377c66b/swarm-container-k8s-9e722acc/deploy/k8s/):
- **Deployment**: 2+ replicas, resource bounds (`requests: 100m/64Mi`, `limits: 500m/256Mi`), rolling updates, and nonroot distroless security.
- **Probes**: Startup, Liveness, and Readiness probes targeting `/health`.
- **Services**: `ClusterIP` (east-west) and `LoadBalancer` (north-south) on port `20127`.
- **Autoscaler**: `HorizontalPodAutoscaler` targeting 70% CPU utilization.
- **Configuration**: ConfigMap and Secret templates.

Deploy with Kustomize:
```bash
kubectl apply -k deploy/k8s/
```

---

## Update to latest

```bash
docker pull mihir0209/9router:latest
docker rm -f 9router
# re-run the quick start command
```

To pin a specific version instead of following `latest`, use a numbered image tag:

```bash
docker pull mihir0209/9router:v0.6.0
```

---

# 🛠 For Developers

## Build image locally (test)

Both `Dockerfile` and `Dockerfile.distroless` perform a self-contained multi-stage static musl Rust build packaged into `gcr.io/distroless/static-debian12:nonroot` (UID/GID `65532`, `<25 MB`):

```bash
docker build -f Dockerfile.distroless -t 9router:distroless-test .

docker run --rm -p 20127:20127 \
  -v "$HOME/.9router:/app/data" \
  -e DATA_DIR=/app/data \
  9router:distroless-test
```

To run automated image inspection, cold-boot timing, memory usage, and HTTP latency benchmarks against Docker Compose:

```bash
node scripts/benchmark-docker.mjs --profile sqlite
```

The Dockerfile uses the official Alpine mirror by default for UPX compression in the prep stage. A regional mirror can be supplied when needed:

```bash
docker build \
  --build-arg ALPINE_MIRROR=mirrors.aliyun.com \
  -f Dockerfile.distroless \
  -t 9router:distroless-test .
```

## Publish (automatic via CI)

Push a Docker-safe semver git tag `vX.Y.Z` (or a prerelease such as `vX.Y.Z-rc.1`) → GitHub Actions builds `linux/amd64` and `linux/arm64` on native runners, health-checks each platform image, verifies the resulting manifest and `/api/health`, then publishes:

- `ghcr.io/mihir0209/9router:X.Y.Z` + `:latest`
- `mihir0209/9router:X.Y.Z` + `:latest`

The `v` prefix is used only for the git tag; image tags omit it. A stable tag push promotes `latest`, but a prerelease tag such as `vX.Y.Z-rc.1` publishes only its numbered image by default. Prereleases require an explicit manual `promote_latest` opt-in. Promotion happens only after both native platform builds, both platform health checks, manifest inspection, and the resolved-manifest smoke test succeed. A failed or timed-out platform build therefore cannot move `latest`.

The workflow rejects SemVer build metadata such as `v1.2.3+build.7` because the `+` form is not a valid Docker image tag. The git tag and both `package.json` versions must match exactly.

```bash
# Use scripts/release.js (recommended)
node scripts/release.js "Release title" "Notes"

# Or manually
git tag v0.5.81 && git push origin v0.5.81
```

To republish an existing tag, run the `Build and Push Docker Image` workflow manually and provide the exact tag, for example `v0.5.81`, in the `release_tag` input. Manual runs publish the numbered tag but leave `latest` unchanged by default:

```text
release_tag:     v0.5.81
promote_latest:  false
```

The `promote_latest` checkbox is an explicit opt-in for changing `latest`. Use it when a deliberate rollback or recovery should make that version the current default:

```text
release_tag:     v0.5.75
promote_latest:  true
```

Numbered image tags are mutable because a republish can replace their manifest. For a deployment that must be immutable, pin the image digest instead:

```bash
docker pull mihir0209/9router@sha256:<verified-digest>
```

The release workflow builds `Dockerfile.distroless` for `linux/amd64` and `linux/arm64` on native runners, verifies non-root UID `65532` and the `<25 MB` image budget, and probes `/health` and `/api/health` on port `20127` on each platform image before assembling the multi-platform manifest. It then runs a second health check against the resolved version manifest before any requested `latest` promotion.

During recovery, the selected tag remains the application source while the Dockerfile from the workflow revision is used, so an older tag can be rebuilt with the current publishing fixes.

The workflow is tag-driven. Creating a git tag does not automatically create a GitHub Release, so the Releases page and the published package/image tags can be at different versions unless a maintainer creates a release separately.

The upstream repository needs these repository secrets for Docker Hub publishing:

- `DOCKERHUB_USERNAME`
- `DOCKERHUB_TOKEN`

GHCR publishing uses the workflow's `GITHUB_TOKEN` with package write permission. Docker Hub publication publishes directly to `mihir0209/9router`.

The optional repository variable `ALPINE_MIRROR` can override the default Alpine mirror used during the UPX prep stage of the CI Docker build.

Workflow: `.github/workflows/docker-publish.yml`
