#!/usr/bin/env bash
set -euo pipefail

REPO="Facto-to/facto-cli"
BIN_NAME="facto"
INSTALL_DIR="$HOME/.facto/bin"

# ---------------------------------------------------------------------------
# Detect OS and architecture
# ---------------------------------------------------------------------------
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
  darwin) OS="darwin" ;;
  linux)  OS="linux" ;;
  *)
    echo "Unsupported OS: $OS" >&2
    exit 1
    ;;
esac

case "$ARCH" in
  x86_64)          ARCH="x86_64" ;;
  aarch64 | arm64) ARCH="aarch64" ;;
  *)
    echo "Unsupported architecture: $ARCH" >&2
    exit 1
    ;;
esac

# ---------------------------------------------------------------------------
# Resolve latest release tag
# ---------------------------------------------------------------------------
RELEASE_URL="https://github.com/${REPO}/releases/latest/download"
BINARY_FILENAME="${BIN_NAME}-${OS}-${ARCH}"

DOWNLOAD_URL="${RELEASE_URL}/${BINARY_FILENAME}"

# ---------------------------------------------------------------------------
# Download and install
# ---------------------------------------------------------------------------
echo "Detected: ${OS}/${ARCH}"
echo "Downloading Facto CLI from ${DOWNLOAD_URL} ..."

mkdir -p "$INSTALL_DIR"

if command -v curl &>/dev/null; then
  curl -fsSL "$DOWNLOAD_URL" -o "${INSTALL_DIR}/${BIN_NAME}"
elif command -v wget &>/dev/null; then
  wget -q "$DOWNLOAD_URL" -O "${INSTALL_DIR}/${BIN_NAME}"
else
  echo "Error: curl or wget is required." >&2
  exit 1
fi

chmod +x "${INSTALL_DIR}/${BIN_NAME}"

# ---------------------------------------------------------------------------
# Success message
# ---------------------------------------------------------------------------
echo ""
echo "Facto CLI installed to ${INSTALL_DIR}/${BIN_NAME}"
echo ""
echo "Get started:"
echo "  \"${INSTALL_DIR}/${BIN_NAME}\" login      # Authenticate (opens browser)"
echo "  \"${INSTALL_DIR}/${BIN_NAME}\" whoami     # Verify login"
echo "  \"${INSTALL_DIR}/${BIN_NAME}\" pipelines  # Check available DeFi balance"
echo "  \"${INSTALL_DIR}/${BIN_NAME}\" fund --amount 10  # Withdraw USDC from DeFi position"
echo ""
echo "Use full absolute paths (do not rely on \$PATH)."
