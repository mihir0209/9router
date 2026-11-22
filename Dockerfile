# syntax=docker/dockerfile:1.7
# 9Router Native Standalone Container
# Single zero-dependency native Rust binary with embedded Next.js dashboard

FROM ubuntu:24.04

LABEL org.opencontainers.image.title="9router" \
      org.opencontainers.image.description="Single-binary AI Gateway, Token Saver & CLI Daemon Manager" \
      org.opencontainers.image.version="0.6.0" \
      org.opencontainers.image.authors="mihir0209"

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      ca-certificates \
      tzdata \
      curl && \
    rm -rf /var/lib/apt/lists/* && \
    mkdir -p /app/data && \
    useradd -u 10001 -d /app -s /bin/sh router && \
    chown -R router:router /app

WORKDIR /app

# Copy native single binary and symlink primary and alias commands
COPY release_assets/9r-linux-x64 /usr/local/bin/9r
RUN chmod +x /usr/local/bin/9r && \
    ln -sf /usr/local/bin/9r /usr/local/bin/9router

USER router

ENV HOST=0.0.0.0
ENV PORT=20128
ENV DATA_DIR=/app/data
ENV STANDALONE=1

EXPOSE 20128
VOLUME ["/app/data"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
  CMD curl -fs http://127.0.0.1:20128/api/auth/status || exit 1

ENTRYPOINT ["9r", "run"]
