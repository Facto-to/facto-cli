#!/usr/bin/env bash
set -euo pipefail

FACTO_BIN="${FACTO_BIN:-$(command -v facto 2>/dev/null || echo "$HOME/.facto/bin/facto")}"
API_URL="${FACTO_API_URL:-http://127.0.0.1:8080}"
USER_ID="${FACTO_MPP_TEST_USER_ID:-mpp-local-user}"
PAY_URL="${FACTO_MPP_TEST_URL:-http://127.0.0.1:8788/api/price}"

FACTO_API_URL="$API_URL" "$FACTO_BIN" login --dev-token "dev:${USER_ID}" >/dev/null

RESULT="$(
  FACTO_API_URL="$API_URL" \
  "$FACTO_BIN" -t pay GET "$PAY_URL"
)"

RESULT_JSON="$RESULT" python3 - <<'PY'
import json
import os

data = json.loads(os.environ["RESULT_JSON"])
assert data["status"] == "paid", data
assert data["protocol"] == "mpp", data
assert data["payment"]["method"] == "monad", data
assert data["default_pipeline_unchanged"] is True, data
print("ok")
PY
