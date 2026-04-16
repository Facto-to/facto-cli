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
# Add to PATH (if not already present)
# ---------------------------------------------------------------------------
add_to_path() {
  local shell_name="$1"
  local rc_file="$2"

  if [ ! -f "$rc_file" ]; then
    return
  fi

  if grep -q "${INSTALL_DIR}" "$rc_file" 2>/dev/null; then
    return  # already configured
  fi

  echo "" >> "$rc_file"
  echo "# Facto CLI" >> "$rc_file"
  echo "export PATH=\"${INSTALL_DIR}:\$PATH\"" >> "$rc_file"
  echo "  Added to ${rc_file}"
}

ADDED_PATH=false

case "$(basename "${SHELL:-/bin/bash}")" in
  zsh)
    if ! grep -q "${INSTALL_DIR}" "$HOME/.zshrc" 2>/dev/null; then
      add_to_path "zsh" "$HOME/.zshrc"
      ADDED_PATH=true
    fi
    ;;
  bash)
    for rc in "$HOME/.bashrc" "$HOME/.bash_profile"; do
      if [ -f "$rc" ] && ! grep -q "${INSTALL_DIR}" "$rc" 2>/dev/null; then
        add_to_path "bash" "$rc"
        ADDED_PATH=true
        break
      fi
    done
    ;;
esac

# ---------------------------------------------------------------------------
# Success message
# ---------------------------------------------------------------------------
echo ""
echo "Facto CLI installed to ${INSTALL_DIR}/${BIN_NAME}"

if [ "$ADDED_PATH" = true ]; then
  echo ""
  echo "PATH updated. Run this to use facto in the current shell:"
  echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
fi

echo ""
echo "Operator setup:"
echo "  facto login             # Authenticate (opens browser)"
echo "  facto pipeline create   # Create your Base payment pipeline"
echo "  facto balance           # Check server wallet USDC"
echo "  facto pipelines         # Inspect pipelines and balances"
echo ""
echo "Agent handoff:"
echo "  https://monad-api.facto.to/SKILL.md"
echo "  codex \"Use facto-cli plus SKILL.md to find a low-cost BTC price API, pay for it, and return only JSON\""
echo ""
echo "Manual path:"
echo "  facto services \"crypto\""
echo "  facto pay GET \"<url>\" --max-amount 0.001 --yes"
echo ""
