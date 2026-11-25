# syntax=docker/dockerfile:1.7
# ==============================================================================
# 9Router Production Multi-Stage Distroless Dockerfile
# Builds static musl Rust binary from source and packages into
# gcr.io/distroless/static-debian12:nonroot (UID/GID 65532)
# Target final uncompressed image size: <25 MB
# ==============================================================================

# ------------------------------------------------------------------------------
# Stage 1: Static Musl Rust Builder (amd64 & arm64 native)
# ------------------------------------------------------------------------------
FROM rust:1-bookworm AS builder

ARG TARGETARCH

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      musl-tools \
      binutils && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/9router

# Copy Cargo manifests, Rust sources, and embedded Next.js dashboard UI
COPY rust/app/Cargo.toml rust/app/Cargo.lock ./rust/app/
COPY rust/app/src ./rust/app/src
COPY rust/app/ui ./rust/app/ui

WORKDIR /usr/src/9router/rust/app

# Compile static musl binary with release LTO and strip debug symbols
RUN set -eux; \
    arch="${TARGETARCH:-$(uname -m)}"; \
    case "$arch" in \
      amd64|x86_64) RUST_TARGET="x86_64-unknown-linux-musl"; export CC_x86_64_unknown_linux_musl=musl-gcc ;; \
      arm64|aarch64) RUST_TARGET="aarch64-unknown-linux-musl"; export CC_aarch64_unknown_linux_musl=musl-gcc ;; \
      *) echo "Unsupported architecture: $arch" >&2; exit 1 ;; \
    esac; \
    rustup target add "$RUST_TARGET"; \
    cargo build --release --target "$RUST_TARGET"; \
    cp "target/${RUST_TARGET}/release/9r" /usr/local/bin/9r; \
    strip --strip-all /usr/local/bin/9r

# ------------------------------------------------------------------------------
# Stage 2: Binary compression (UPX), filesystem initialization & symlinks
# ------------------------------------------------------------------------------
FROM alpine:3.20 AS prep

ARG ALPINE_MIRROR=dl-cdn.alpinelinux.org

RUN sed -i "s|dl-cdn.alpinelinux.org|${ALPINE_MIRROR}|g" /etc/apk/repositories && \
    apk add --no-cache upx && \
    mkdir -p /app/data /usr/local/bin

COPY --from=builder /usr/local/bin/9r /usr/local/bin/9r

RUN upx --best --lzma /usr/local/bin/9r && \
    chmod +x /usr/local/bin/9r && \
    ln -s /usr/local/bin/9r /usr/local/bin/9router && \
    ln -s /usr/local/bin/9r /app/9r && \
    ln -s /usr/local/bin/9r /app/ninerouter && \
    chown -R 65532:65532 /app /usr/local/bin

# ------------------------------------------------------------------------------
# Stage 3: Distroless Static Minimal Runtime (<25MB)
# ------------------------------------------------------------------------------
FROM gcr.io/distroless/static-debian12:nonroot

ARG APP_VERSION=0.6.0

LABEL org.opencontainers.image.title="9router" \
      org.opencontainers.image.description="Single-binary AI Gateway, Token Saver & CLI Daemon Manager" \
      org.opencontainers.image.version="${APP_VERSION}" \
      org.opencontainers.image.authors="mihir0209" \
      org.opencontainers.image.source="https://github.com/mihir0209/9router" \
      org.opencontainers.image.licenses="MIT"

WORKDIR /app

# Copy prepared directory structure and single binary + symlinks with nonroot ownership (UID/GID 65532)
COPY --from=prep --chown=65532:65532 /app /app
COPY --from=prep --chown=65532:65532 /usr/local/bin /usr/local/bin

USER 65532:65532

ENV HOST=0.0.0.0 \
    PORT=20127 \
    DATA_DIR=/app/data \
    STANDALONE=1 \
    NODE_ENV=production

EXPOSE 20127

VOLUME ["/app/data"]

# Native zero-overhead latency probe healthcheck
HEALTHCHECK --interval=15s --timeout=3s --start-period=5s --retries=3 \
  CMD ["/usr/local/bin/9r", "test", "-p", "20127", "-n", "1"]

ENTRYPOINT ["/usr/local/bin/9r"]
CMD ["run"]
