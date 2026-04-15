#!/usr/bin/env bash
#
# Facto CLI — Smoke & Integration Test Suite
#
# Runs against a live Facto API environment to verify end-to-end correctness.
#
# Usage:
#   ./tests/smoke.sh              # test against current env (default: dev)
#   ./tests/smoke.sh dev          # explicit dev environment
#   ./tests/smoke.sh prod         # explicit prod environment
#   ./tests/smoke.sh local        # test against local engine (localhost:8080)
#
# Prerequisites:
#   - facto binary installed (~/.facto/bin/facto or in PATH)
#   - Authenticated: `facto login` completed
#   - For pay test: server wallet has USDC balance on Base (8453)
#
# Exit codes:
#   0 — all tests passed
#   1 — one or more tests failed
#
set -euo pipefail

# ── Config ──────────────────────────────────────────────────────────────

ENV="${1:-}"
FACTO="${FACTO_BIN:-$(command -v facto 2>/dev/null || echo "$HOME/.facto/bin/facto")}"
PASS=0
FAIL=0
SKIP=0
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
NC='\033[0m'

# Resolve API URL
case "$ENV" in
  local)  export FACTO_API_URL="http://localhost:8080" ;;
  dev)    export FACTO_API_URL="https://monad-api.facto.to" ;;
  prod)   export FACTO_API_URL="https://api.facto.xyz" ;;
  "")     ;; # use whatever is configured
esac

API_URL="${FACTO_API_URL:-$("$FACTO" -t config --show 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)['api_url'])" 2>/dev/null || echo "unknown")}"

# ── Helpers ─────────────────────────────────────────────────────────────

pass() { ((PASS++)); echo -e "  ${GREEN}✅ PASS${NC}  $1"; }
fail() { ((FAIL++)); echo -e "  ${RED}❌ FAIL${NC}  $1 — $2"; }
skip() { ((SKIP++)); echo -e "  ${YELLOW}⏭  SKIP${NC}  $1 — $2"; }

# Run a command, capture stdout+stderr, check exit code
run() {
  local output
  output=$("$@" 2>&1) && echo "$output" || { echo "$output"; return 1; }
}

# Parse JSON field from terse output
jq_field() {
  python3 -c "import sys,json; d=json.load(sys.stdin); print(d$1)" 2>/dev/null
}

# ── Banner ──────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}Facto CLI Integration Tests${NC}"
echo -e "Binary:  $FACTO"
echo -e "API:     $API_URL"
echo -e "Env:     ${ENV:-auto}"
echo "──────────────────────────────────────────"

# ── Test 1: Health ──────────────────────────────────────────────────────

echo -e "\n${BOLD}[1/9] Health Check${NC}"
HEALTH=$(curl -sf "$API_URL/health" 2>/dev/null || echo "")
if [ "$HEALTH" = "ok" ]; then
  pass "API health endpoint returns ok"
else
  fail "API health endpoint" "got '$HEALTH'"
fi

# ── Test 2: Auth ────────────────────────────────────────────────────────

echo -e "\n${BOLD}[2/9] Authentication${NC}"
WHOAMI=$(run "$FACTO" -t whoami 2>/dev/null || echo "")
if echo "$WHOAMI" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d['authenticated']==True" 2>/dev/null; then
  USER_ID=$(echo "$WHOAMI" | jq_field "['user_id']")
  pass "Authenticated as $USER_ID"
else
  fail "Authentication" "not logged in — run 'facto login' first"
  echo -e "\n${RED}Cannot continue without authentication. Aborting.${NC}\n"
  exit 1
fi

# ── Test 3: Pipelines ──────────────────────────────────────────────────

echo -e "\n${BOLD}[3/9] Pipelines${NC}"
PIPELINES=$(run "$FACTO" -t pipelines 2>/dev/null || echo "")
PIPE_COUNT=$(echo "$PIPELINES" | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('pipelines',[])))" 2>/dev/null || echo "0")
if [ "$PIPE_COUNT" -gt 0 ]; then
  FIRST_BALANCE=$(echo "$PIPELINES" | python3 -c "import sys,json; p=json.load(sys.stdin)['pipelines'][0]; print(f\"{p['balance']} {p['asset_symbol']} on chain {p['chain_id']}\")" 2>/dev/null)
  pass "Found $PIPE_COUNT pipeline(s) — $FIRST_BALANCE"
else
  skip "Pipelines" "no active pipelines found"
fi

# ── Test 4: Balance ─────────────────────────────────────────────────────

echo -e "\n${BOLD}[4/9] Server Wallet Balance${NC}"
BALANCE=$(run "$FACTO" -t balance --chain 8453 2>/dev/null || echo "")
BAL_DISPLAY=$(echo "$BALANCE" | jq_field "['usdc_balance_display']" 2>/dev/null || echo "")
WALLET=$(echo "$BALANCE" | jq_field "['wallet_address']" 2>/dev/null || echo "")
if [ -n "$BAL_DISPLAY" ] && [ "$BAL_DISPLAY" != "None" ]; then
  pass "Balance: $BAL_DISPLAY (wallet: ${WALLET:0:10}...)"
else
  fail "Balance check" "could not fetch balance"
fi

# ── Test 5: Service Discovery ───────────────────────────────────────────

echo -e "\n${BOLD}[5/9] Service Discovery${NC}"
SERVICES=$(run "$FACTO" -t services "weather" 2>/dev/null || echo "")
SVC_COUNT=$(echo "$SERVICES" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "0")
if [ "$SVC_COUNT" -gt 0 ]; then
  FIRST_URL=$(echo "$SERVICES" | python3 -c "import sys,json; print(json.load(sys.stdin)[0]['url'])" 2>/dev/null)
  pass "Found $SVC_COUNT service(s) — $FIRST_URL"
else
  fail "Service discovery" "no services found for 'weather'"
fi

# ── Test 6: x402 Pay ───────────────────────────────────────────────────

echo -e "\n${BOLD}[6/9] x402 Payment (live)${NC}"

# Check balance sufficient (need at least $0.01 = 10000 raw units)
RAW_BAL=$(echo "$BALANCE" | python3 -c "import sys,json; print(int(json.load(sys.stdin).get('usdc_balance','0')))" 2>/dev/null || echo "0")
if [ "$RAW_BAL" -lt 5000 ]; then
  skip "x402 pay" "insufficient balance ($BAL_DISPLAY) — need at least \$0.005"
else
  # Use a known cheap service
  PAY_URL="https://x402.aurelianflo.com/api/weather/current?lat=40.7&lon=-74.0"
  PAY_RESULT=$(run "$FACTO" -t pay GET "$PAY_URL" --chain 8453 2>/dev/null || echo "{}")
  PAY_STATUS=$(echo "$PAY_RESULT" | jq_field "['status']" 2>/dev/null || echo "")
  PAY_HTTP=$(echo "$PAY_RESULT" | jq_field "['http_status']" 2>/dev/null || echo "0")
  CHARGE_ID=$(echo "$PAY_RESULT" | jq_field "['payment']['charge_id']" 2>/dev/null || echo "")

  if [ "$PAY_STATUS" = "paid" ] && [ "$PAY_HTTP" = "200" ]; then
    pass "Paid \$0.005, HTTP 200, charge: $CHARGE_ID"
  elif [ "$PAY_STATUS" = "paid" ]; then
    fail "x402 pay" "payment sent but response HTTP $PAY_HTTP (expected 200)"
  elif [ "$PAY_STATUS" = "free" ]; then
    pass "Free response (no payment required) — HTTP $PAY_HTTP"
  else
    ERROR=$(echo "$PAY_RESULT" | jq_field "['error']" 2>/dev/null || echo "unknown")
    fail "x402 pay" "$ERROR"
  fi
fi

# ── Test 7: History ─────────────────────────────────────────────────────

echo -e "\n${BOLD}[7/9] Transaction History${NC}"
HISTORY=$(run "$FACTO" -t history 2>/dev/null || echo "")
HIST_COUNT=$(echo "$HISTORY" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "0")
if [ "$HIST_COUNT" -gt 0 ]; then
  pass "Found $HIST_COUNT charge(s)"
else
  skip "History" "no charge history"
fi

# ── Test 8: Fund Dry-Run ───────────────────────────────────────────────

echo -e "\n${BOLD}[8/9] Fund Dry-Run${NC}"
if [ "$PIPE_COUNT" -gt 0 ]; then
  # Pick the first pipeline ID for dry-run (handles multi-pipeline accounts)
  FIRST_ROUTE=$(echo "$PIPELINES" | python3 -c "import sys,json; print(json.load(sys.stdin)['pipelines'][0]['route_id'])" 2>/dev/null || echo "")
  FUND_DRY=$(run "$FACTO" -t fund --amount 0.01 --pipeline "$FIRST_ROUTE" --dry-run 2>/dev/null || echo "{}")
  IS_DRY=$(echo "$FUND_DRY" | jq_field "['dry_run']" 2>/dev/null || echo "")
  if [ "$IS_DRY" = "True" ]; then
    ROUTE=$(echo "$FUND_DRY" | jq_field "['route_id']" 2>/dev/null || echo "?")
    pass "Dry-run OK — route: $ROUTE"
  else
    fail "Fund dry-run" "unexpected response"
  fi
else
  skip "Fund dry-run" "no active pipelines"
fi

# ── Test 9: Local MPP Auto Pay ─────────────────────────────────────────

echo -e "\n${BOLD}[9/9] Local MPP Auto Pay${NC}"
if [ "${ENV:-}" = "local" ]; then
  LOCAL_MPP_OUT="$(mktemp)"
  if "$SCRIPT_DIR/mpp_local.sh" >"$LOCAL_MPP_OUT" 2>&1; then
    pass "Local MPP auto pay"
  else
    OUTPUT="$(cat "$LOCAL_MPP_OUT")"
    fail "Local MPP auto pay" "$OUTPUT"
  fi
  rm -f "$LOCAL_MPP_OUT"
else
  skip "Local MPP auto pay" "only runs against local engine"
fi

# ── Summary ─────────────────────────────────────────────────────────────

echo ""
echo "──────────────────────────────────────────"
TOTAL=$((PASS + FAIL + SKIP))
echo -e "${BOLD}Results:${NC} $TOTAL tests — ${GREEN}$PASS passed${NC}, ${RED}$FAIL failed${NC}, ${YELLOW}$SKIP skipped${NC}"
echo ""

if [ "$FAIL" -gt 0 ]; then
  echo -e "${RED}FAILED${NC}"
  exit 1
else
  echo -e "${GREEN}ALL PASSED${NC}"
  exit 0
fi
