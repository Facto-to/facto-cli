#!/usr/bin/env bash
# scripts/dev/preflight.sh — env-var presence check for facto-cli.
#
# The CLI reads almost no env vars at runtime, so preflight is mostly a
# placeholder confirming nothing is required by default.
set -euo pipefail

cd "$(dirname "$0")/../.."

if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
  C_RESET=$'\033[0m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'; C_RED=$'\033[31m'; C_BOLD=$'\033[1m'
else
  C_RESET=""; C_GREEN=""; C_YELLOW=""; C_RED=""; C_BOLD=""
fi
ok()   { printf '  %s✓%s %s\n' "$C_GREEN" "$C_RESET" "$*"; }
warn() { printf '  %s!%s %s\n' "$C_YELLOW" "$C_RESET" "$*"; }
group(){ printf '\n%s[%s]%s\n' "$C_BOLD" "$*" "$C_RESET"; }

# Layered load: .env -> .env.local -> shell
for f in .env .env.local; do
  if [[ -f "$f" ]]; then
    set -a; source "$f"; set +a
  fi
done

group "env"
if [[ -n "${FACTO_API_URL:-}" ]]; then
  ok "FACTO_API_URL=$FACTO_API_URL (override active)"
else
  ok "FACTO_API_URL not set (CLI will use the default production backend)"
fi

printf '\n%ssummary%s: nothing required for default operation\n' "$C_BOLD" "$C_RESET"
exit 0
