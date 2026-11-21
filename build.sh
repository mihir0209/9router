#!/usr/bin/env bash
set -euo pipefail

# Build script for 9Router Native Rust Core + Embedded React Dashboard

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$PROJECT_ROOT"

echo "===================================================="
echo "⚡ Building 9Router Native Core (Single-Binary Rust)"
echo "===================================================="

# Step 1: Build the React Dashboard if needed
if [ ! -f "web/dist/index.html" ] || [ "${1:-}" = "--rebuild-web" ]; then
    echo "📦 [1/2] Building React Dashboard SPA in web/..."
    cd web
    if [ ! -d "node_modules" ]; then
        echo "Installing web dependencies..."
        npm install --silent
    fi
    npm run build
    cd "$PROJECT_ROOT"
else
    echo "✅ [1/2] React Dashboard SPA assets found in web/dist/"
fi

# Step 2: Build the Rust binary with embedded assets
echo "🦀 [2/2] Compiling native Rust binary with rust-embed..."
cargo build --release --manifest-path rust/app/Cargo.toml

BINARY="$PROJECT_ROOT/rust/app/target/release/ninerouter"
if [ -f "$BINARY" ]; then
    SIZE=$(ls -lh "$BINARY" | awk '{print $5}')
    echo "===================================================="
    echo "🎉 Successfully built 9Router standalone binary!"
    echo "   Binary location: $BINARY"
    echo "   Binary size:     $SIZE"
    echo "   Port:            20127 (configurable via PORT env)"
    echo "===================================================="
else
    echo "❌ Build failed: binary not found at $BINARY"
    exit 1
fi
