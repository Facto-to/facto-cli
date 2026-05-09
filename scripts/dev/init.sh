#!/usr/bin/env bash
# scripts/dev/init.sh — first-day contributor orchestrator for facto-cli.
#
# Steps:
#   1. verify cargo / rustc installed (MSRV 1.75)
#   2. cargo check
#   3. cargo test
#   4. ensure .env.local exists (optional — only used to override defaults)
#   5. preflight + next-step prompt
set -euo pipefail

cd "$(dirname "$0")/../.."

if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
  C_RESET=$'\033[0m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'; C_RED=$'\033[31m'; C_BOLD=$'\033[1m'
else
  C_RESET=""; C_GREEN=""; C_YELLOW=""; C_RED=""; C_BOLD=""
fi
ok()   { printf '%s✓%s %s\n' "$C_GREEN" "$C_RESET" "$*"; }
warn() { printf '%s!%s %s\n' "$C_YELLOW" "$C_RESET" "$*"; }
fail() { printf '%s✗%s %s\n' "$C_RED" "$C_RESET" "$*"; }
group(){ printf '\n%s[%s]%s\n' "$C_BOLD" "$*" "$C_RESET"; }

skip_test=0
for arg in "$@"; do
  case "$arg" in
    --skip-test) skip_test=1 ;;
    -h|--help) cat <<EOF; exit 0
init.sh — facto-cli first-day onboarding

  --skip-test    skip cargo test (faster init)
EOF
      ;;
  esac
done

printf '%s== facto-cli dev init ==%s\n' "$C_BOLD" "$C_RESET"

# Step 1
group "rust toolchain"
if command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1; then
  ok "$(rustc --version)"
  ok "$(cargo --version)"
else
  fail "rustup not installed — see https://rustup.rs/"
  exit 1
fi

# Step 2
group "cargo check"
if cargo check --all-targets 2>&1 | tail -5; then
  ok "cargo check OK"
else
  fail "cargo check failed"; exit 1
fi

# Step 3
if [[ "$skip_test" -eq 0 ]]; then
  group "cargo test"
  if cargo test --quiet 2>&1 | tail -5; then
    ok "cargo test OK"
  else
    warn "cargo test had failures — review output above"
  fi
else
  warn "skipping cargo test (--skip-test)"
fi

# Step 4
group ".env.local"
if [[ -f .env.local ]]; then
  ok ".env.local exists"
else
  cp .env.example .env.local
  warn "created .env.local from .env.example template (override FACTO_API_URL only if dev'ing against non-default backend)"
fi

# Step 5
group "preflight"
scripts/dev/preflight.sh

printf '\n%snext steps%s:\n' "$C_BOLD" "$C_RESET"
cat <<EOF

  Run the CLI in dev:
    cargo run -- --help                # see subcommands
    cargo run -- login                  # local login flow
    cargo run -- balance -t             # balance probe (testnet)

  Build release binary:
    cargo build --release
    target/release/facto --help

  Or use the public install for production parity:
    bash install.sh                     # local install (mirrors public flow)
EOF
