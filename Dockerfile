# syntax=docker/dockerfile:1.7
# ==============================================================================
# 9Router Distroless Container (<25MB)
# Production multi-stage packaging using gcr.io/distroless/static-debian12:nonroot
# ==============================================================================

# Stage 1: Filesystem initialization & symlink creation
FROM alpine:3.20 AS prep
RUN mkdir -p /app/data /usr/local/bin
COPY release_assets/9r-linux-x64 /usr/local/bin/9r
RUN chmod +x /usr/local/bin/9r && \
    ln -s /usr/local/bin/9r /usr/local/bin/9router && \
    ln -s /usr/local/bin/9r /app/9r && \
    ln -s /usr/local/bin/9r /app/ninerouter && \
    chown -R 65532:65532 /app /usr/local/bin

# Stage 2: Distroless Static Minimal Runtime (<25MB target, ~13.8MB actual)
FROM gcr.io/distroless/static-debian12:nonroot

LABEL org.opencontainers.image.title="9router" \
      org.opencontainers.image.description="Single-binary AI Gateway, Token Saver & CLI Daemon Manager" \
      org.opencontainers.image.version="0.6.0" \
      org.opencontainers.image.authors="mihir0209" \
      org.opencontainers.image.source="https://github.com/mihir0209/9router" \
      org.opencontainers.image.licenses="MIT"

WORKDIR /app

# Copy prepared directory structure and single binary + symlinks with nonroot ownership
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
