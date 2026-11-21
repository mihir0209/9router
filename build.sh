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
echo "🦀 [2/2] Compiling native Rust binary with rust-embed..."
cargo build --release --manifest-path rust/app/Cargo.toml

BINARY="$PROJECT_ROOT/rust/app/target/release/9r"
if [ ! -f "$BINARY" ]; then
    BINARY="$PROJECT_ROOT/rust/app/target/release/ninerouter"
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
