#!/usr/bin/env bash
set -euo pipefail

# Build script for 9Router Native Rust Core + Embedded Authentic Next.js React Dashboard

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$PROJECT_ROOT"

echo "===================================================="
echo "⚡ Building 9Router Native Core (Single-Binary Rust)"
echo "===================================================="

# Step 1: Assemble the Authentic Next.js Dashboard UI if needed
if [ ! -d "rust/app/ui" ] || [ ! -f "rust/app/ui/dashboard.html" ] || [ "${1:-}" = "--rebuild-ui" ]; then
    echo "📦 [1/2] Building authentic 9Router Next.js Dashboard..."
    if [ ! -d ".next" ] || [ "${1:-}" = "--rebuild-ui" ]; then
        npm run build
    fi
    node scripts/build-embedded-ui.mjs
else
    echo "✅ [1/2] Authentic 9Router Next.js Dashboard assets found in rust/app/ui/"
fi

# Step 2: Build the Rust binary with embedded assets
TARGET_FLAG=""
if [ "${1:-}" = "--musl" ] || [ "${1:-}" = "--distroless" ] || [ "${2:-}" = "--musl" ] || [ "${2:-}" = "--distroless" ]; then
    TARGET_FLAG="--target x86_64-unknown-linux-musl"
    echo "🦀 [2/2] Compiling static musl Rust binary ($TARGET_FLAG)..."
else
    echo "🦀 [2/2] Compiling native Rust binary with rust-embed..."
fi

cargo build --release $TARGET_FLAG --manifest-path rust/app/Cargo.toml

if [ -n "$TARGET_FLAG" ]; then
    BINARY="$PROJECT_ROOT/rust/app/target/x86_64-unknown-linux-musl/release/9r"
    if [ ! -f "$BINARY" ]; then
        BINARY="$PROJECT_ROOT/rust/app/target/x86_64-unknown-linux-musl/release/ninerouter"
    fi
    mkdir -p "$PROJECT_ROOT/release_assets"
    cp "$BINARY" "$PROJECT_ROOT/release_assets/9r-linux-x64"
    if which upx >/dev/null 2>&1; then
        upx --best --lzma "$PROJECT_ROOT/release_assets/9r-linux-x64" 2>/dev/null || true
    fi
else
    BINARY="$PROJECT_ROOT/rust/app/target/release/9r"
    if [ ! -f "$BINARY" ]; then
        BINARY="$PROJECT_ROOT/rust/app/target/release/ninerouter"
    fi
fi

if [ -f "$BINARY" ]; then
    SIZE=$(ls -lh "$BINARY" | awk '{print $5}')
    echo "===================================================="
    echo "🎉 Successfully built authentic 9Router standalone binary!"
    echo "   Binary location: $BINARY"
    echo "   Binary size:     $SIZE"
    echo "   Port:            20127 (configurable via PORT env)"
    echo "===================================================="
else
    echo "❌ Build failed: binary not found at $BINARY"
    exit 1
fi
