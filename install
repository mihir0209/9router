#!/usr/bin/env bash
# Universal cross-platform installer for 9Router (9r)
# Supported OS: Linux, macOS, Windows (Git Bash, MSYS2, WSL)
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/mihir0209/9router/master/install | bash
#   ./scripts/install.sh [options]

set -euo pipefail

REPO="mihir0209/9router"
COMMAND_PRIMARY="9r"
COMMAND_ALIAS="9router"
DEFAULT_INSTALL_DIR="$HOME/.local/bin"

TARGET_VERSION=""
LOCAL_BINARY=""
CUSTOM_INSTALL_DIR=""
NO_MODIFY_PATH=0
VERBOSE=0

# ANSI color codes
reset="\033[0m"
bold="\033[1m"
red="\033[31m"
green="\033[32m"
yellow="\033[33m"
cyan="\033[36m"
dim="\033[2m"

if [ ! -t 1 ]; then
  reset=""
  bold=""
  red=""
  green=""
  yellow=""
  cyan=""
  dim=""
fi

log_info() {
  printf "${cyan}${bold}>${reset} %s\n" "$1"
}

log_success() {
  printf "${green}${bold}✓${reset} %s\n" "$1"
}

log_warn() {
  printf "${yellow}${bold}!${reset} %s\n" "$1"
}

log_error() {
  printf "${red}${bold}x${reset} %s\n" "$1" >&2
}

print_help() {
  cat <<EOF
9Router Universal Installer

Usage:
  install [flags]

Flags:
  --version <v>        Install specific release version (e.g. v0.6.0)
  --binary <path>      Install from local binary file directly (skips download)
  --install-dir <dir>  Target directory for binary (default: ~/.local/bin)
  --no-modify-path     Do not append install directory to shell profile PATH
  -v, --verbose        Enable verbose debug output
  -h, --help           Show this help message
EOF
}

# Parse command line flags
while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      TARGET_VERSION="$2"
      shift 2
      ;;
    --binary)
      LOCAL_BINARY="$2"
      shift 2
      ;;
    --install-dir)
      CUSTOM_INSTALL_DIR="$2"
      shift 2
      ;;
    --no-modify-path)
      NO_MODIFY_PATH=1
      shift
      ;;
    -v|--verbose)
      VERBOSE=1
      shift
      ;;
    -h|--help)
      print_help
      exit 0
      ;;
    *)
      log_warn "Unknown argument: $1"
      shift
      ;;
  esac
done

# Detect operating system
detect_os() {
  local os_raw
  os_raw="$(uname -s 2>/dev/null || echo "unknown")"
  case "$os_raw" in
    Linux)
      echo "linux"
      ;;
    Darwin)
      echo "darwin"
      ;;
    MINGW*|MSYS*|CYGWIN*|Windows_NT)
      echo "windows"
      ;;
    *)
      echo "unknown"
      ;;
  esac
}

# Detect CPU architecture
detect_arch() {
  local arch_raw
  arch_raw="$(uname -m 2>/dev/null || echo "unknown")"
  case "$arch_raw" in
    x86_64|amd64)
      # Check Rosetta 2 emulation on macOS
      if [ "$(uname -s)" = "Darwin" ]; then
        if [ "$(sysctl -in sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then
          echo "arm64"
          return
        fi
      fi
      echo "x64"
      ;;
    aarch64|arm64)
      echo "arm64"
      ;;
    armv7l|armv6l)
      echo "armv7"
      ;;
    *)
      echo "unknown"
      ;;
  esac
}

# Detect C library on Linux (musl vs glibc)
detect_libc() {
  if [ "$(uname -s)" != "Linux" ]; then
    echo "none"
    return
  fi

  if ldd /bin/ls 2>&1 | grep -q 'musl'; then
    echo "musl"
    return
  fi

  if [ -f /etc/alpine-release ]; then
    echo "musl"
    return
  fi

  echo "glibc"
}

OS="$(detect_os)"
ARCH="$(detect_arch)"
LIBC="$(detect_libc)"

if [ "$OS" = "unknown" ] || [ "$ARCH" = "unknown" ]; then
  log_error "Unsupported platform: OS=$(uname -s), Arch=$(uname -m)"
  exit 1
fi

INSTALL_DIR="${CUSTOM_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
mkdir -p "$INSTALL_DIR"

BINARY_NAME="$COMMAND_PRIMARY"
if [ "$OS" = "windows" ]; then
  BINARY_NAME="${COMMAND_PRIMARY}.exe"
fi
TARGET_FILE="$INSTALL_DIR/$BINARY_NAME"

log_info "Detected environment: OS=${OS}, Arch=${ARCH}, Libc=${LIBC}"
log_info "Target install location: ${TARGET_FILE}"

# Step 1: Install binary (Local file or GitHub download)
if [ -n "$LOCAL_BINARY" ]; then
  if [ ! -f "$LOCAL_BINARY" ]; then
    log_error "Local binary not found at: $LOCAL_BINARY"
    exit 1
  fi
  log_info "Installing from local file: $LOCAL_BINARY"
  cp -f "$LOCAL_BINARY" "$TARGET_FILE"
  chmod 755 "$TARGET_FILE"
else
  # Resolve target GitHub release asset
  PLATFORM_TARGET=""
  case "${OS}-${ARCH}" in
    linux-x64)
      PLATFORM_TARGET="9r-linux-x64"
      ;;
    linux-arm64)
      PLATFORM_TARGET="9r-linux-arm64"
      ;;
    darwin-x64)
      PLATFORM_TARGET="9r-darwin-x64"
      ;;
    darwin-arm64)
      PLATFORM_TARGET="9r-darwin-arm64"
      ;;
    windows-x64)
      PLATFORM_TARGET="9r-windows-x64.exe"
      ;;
    *)
      log_error "No prebuilt release binary available for ${OS}-${ARCH}."
      exit 1
      ;;
  esac

  # Determine version
  if [ -z "$TARGET_VERSION" ]; then
    log_info "Fetching latest release tag from GitHub..."
    LATEST_JSON=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null || true)
    if [ -n "$LATEST_JSON" ]; then
      TARGET_VERSION=$(echo "$LATEST_JSON" | grep -o '"tag_name": *"[^"]*"' | head -n1 | cut -d'"' -f4 || echo "")
    fi
  fi

  if [ -z "$TARGET_VERSION" ]; then
    TARGET_VERSION="latest"
  fi

  DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${TARGET_VERSION}/${PLATFORM_TARGET}"
  if [ "$TARGET_VERSION" = "latest" ]; then
    DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${PLATFORM_TARGET}"
  fi

  TMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t '9router-install')"
  trap 'rm -rf "$TMP_DIR"' EXIT

  TMP_DOWNLOAD="$TMP_DIR/$PLATFORM_TARGET"
  log_info "Downloading ${DOWNLOAD_URL}..."

  if command -v curl >/dev/null 2>&1; then
    curl -fL --progress-bar "$DOWNLOAD_URL" -o "$TMP_DOWNLOAD" || {
      log_error "Failed to download binary from $DOWNLOAD_URL"
      exit 1
    }
  elif command -v wget >/dev/null 2>&1; then
    wget -q --show-progress "$DOWNLOAD_URL" -O "$TMP_DOWNLOAD" || {
      log_error "Failed to download binary from $DOWNLOAD_URL"
      exit 1
    }
  else
    log_error "Neither curl nor wget is available. Please install curl."
    exit 1
  fi

  cp -f "$TMP_DOWNLOAD" "$TARGET_FILE"
  chmod 755 "$TARGET_FILE"
fi

# Step 2: Create alias symlink / shim
ALIAS_FILE="$INSTALL_DIR/$COMMAND_ALIAS"
if [ "$OS" = "windows" ]; then
  ALIAS_FILE="$INSTALL_DIR/${COMMAND_ALIAS}.cmd"
  cat <<EOF > "$ALIAS_FILE"
@echo off
"%~dp0${COMMAND_PRIMARY}.exe" %*
EOF
  # Also create a .cmd shim for 9r
  cat <<EOF > "$INSTALL_DIR/${COMMAND_PRIMARY}.cmd"
@echo off
"%~dp0${COMMAND_PRIMARY}.exe" %*
EOF
else
  ln -sf "$COMMAND_PRIMARY" "$ALIAS_FILE"
fi
log_success "Binary installed: $TARGET_FILE"
log_success "Alias created: $ALIAS_FILE"

# Step 3: Configure PATH in user shell profile
modify_profile_path() {
  local profile_file="$1"
  local export_line="export PATH=\"$INSTALL_DIR:\$PATH\""

  if [ -f "$profile_file" ]; then
    if grep -Fqs "$INSTALL_DIR" "$profile_file"; then
      return 0
    fi
  fi

  log_info "Adding $INSTALL_DIR to $profile_file"
  printf "\n# 9Router PATH\n%s\n" "$export_line" >> "$profile_file"
}

if [ "$NO_MODIFY_PATH" -eq 0 ]; then
  case ":$PATH:" in
    *":$INSTALL_DIR:"*)
      # Already in PATH
      ;;
    *)
      log_info "Configuring PATH..."
      # Detect shell profiles
      case "${SHELL:-}" in
        */zsh)
          modify_profile_path "$HOME/.zshrc"
          ;;
        */bash)
          if [ -f "$HOME/.bashrc" ]; then
            modify_profile_path "$HOME/.bashrc"
          elif [ -f "$HOME/.bash_profile" ]; then
            modify_profile_path "$HOME/.bash_profile"
          else
            modify_profile_path "$HOME/.profile"
          fi
          ;;
        */fish)
          FISH_CONF="$HOME/.config/fish/config.fish"
          mkdir -p "$(dirname "$FISH_CONF")"
          if ! grep -Fqs "$INSTALL_DIR" "$FISH_CONF" 2>/dev/null; then
            printf "\n# 9Router PATH\nfish_add_path %s\n" "$INSTALL_DIR" >> "$FISH_CONF"
          fi
          ;;
        *)
          if [ -f "$HOME/.profile" ]; then
            modify_profile_path "$HOME/.profile"
          fi
          ;;
      esac

      # If running on Windows git bash / msys, update User PATH in registry
      if [ "$OS" = "windows" ] && command -v powershell.exe >/dev/null 2>&1; then
        powershell.exe -NoProfile -Command "[Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path', 'User') + ';$INSTALL_DIR', 'User')" 2>/dev/null || true
      fi
      ;;
  esac
fi

# Step 4: Verification banner
printf "\n${green}${bold}9Router (${COMMAND_PRIMARY}) installed successfully!${reset}\n"
printf "\nQuick start:\n"
printf "  ${cyan}%s start -d${reset}       # Start background daemon\n" "$COMMAND_PRIMARY"
printf "  ${cyan}%s status${reset}         # View running gateway status\n" "$COMMAND_PRIMARY"
printf "  ${cyan}%s connect${reset}        # Auto-configure local AI coding tools\n" "$COMMAND_PRIMARY"
printf "  ${cyan}%s --help${reset}         # Show complete command reference\n\n" "$COMMAND_PRIMARY"

if ! echo ":$PATH:" | grep -Fqs ":$INSTALL_DIR:"; then
  printf "${yellow}Note:${reset} Run ${bold}source ~/.bashrc${reset} (or restart your terminal) to apply PATH changes.\n\n"
fi
