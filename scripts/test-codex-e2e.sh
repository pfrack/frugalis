#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# Codex CLI E2E Verification
# ============================================================================
# USAGE:
#   ./scripts/test-codex-e2e.sh              # Run against running Frugalis
#   ./scripts/test-codex-e2e.sh --with-mock  # Start mock upstream + Frugalis
# ============================================================================
#
# Prerequisites:
#   1. Frugalis running on PORT (default 10000) with PROXY_API_BEARER_TOKEN set
#   2. Codex CLI installed (https://github.com/openai/codex)
#   3. jq installed (apt install jq / brew install jq)
#
# This script:
#   1. Verifies the Frugalis /v1/responses endpoint works
#   2. Configures Codex CLI to use Frugalis as the provider
#   3. Runs a simple codex query and checks the exit code

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/.."

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
HOST="${HOST:-127.0.0.1:10000}"
TOKEN="${PROXY_API_BEARER_TOKEN:-test-token}"
PASS=0; FAIL=0

log_pass() { PASS=$((PASS+1)); echo -e "  ${GREEN}✓${NC} $1"; }
log_fail() { FAIL=$((FAIL+1)); echo -e "  ${RED}✗${NC} $1"; }

# ── Step 1: Verify Frugalis is running ──
echo "╔══════════════════════════════════════════════════════════════════╗"
echo "║  Codex CLI E2E Test Suite                                       ║"
echo "╚══════════════════════════════════════════════════════════════════╝"
echo ""

echo "── Step 1: Verify Frugalis health ──"
HEALTH=$(curl -s -o /dev/null -w "%{http_code}" "http://$HOST/health" 2>/dev/null || echo "000")
if [ "$HEALTH" = "200" ]; then
    log_pass "Frugalis is running on $HOST"
else
    log_fail "Frugalis not reachable on $HOST (got HTTP $HEALTH)"
    echo "  Start Frugalis: RUST_LOG=info cargo run"
    echo "  Then re-run this script."
    exit 1
fi

# ── Step 2: Verify /v1/responses endpoint ──
echo ""
echo "── Step 2: Verify /v1/responses endpoint ──"

# Non-streaming
RESP=$(curl -sS -w "\n%{http_code}" "http://$HOST/v1/responses" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    -d '{"model":"gpt-4o","input":"hello"}')
CODE=$(printf '%s' "$RESP" | tail -1)
OBJ=$(printf '%s' "$RESP" | sed '$d' | python3 -c "import sys,json; print(json.load(sys.stdin).get('object',''))" 2>/dev/null || echo "")
if [ "$CODE" = "200" ] && [ "$OBJ" = "response" ]; then
    log_pass "/v1/responses returns valid Response object"
else
    log_fail "/v1/responses expected 200/response, got code=$CODE object=$OBJ"
fi

# Auth required
AUTH_CODE=$(curl -s -o /dev/null -w "%{http_code}" "http://$HOST/v1/responses" \
    -H "Content-Type: application/json" \
    -d '{"model":"gpt-4o","input":"hello"}' 2>/dev/null) || true
if [ "$AUTH_CODE" = "401" ]; then
    log_pass "/v1/responses requires auth (401 without token)"
else
    log_fail "/v1/responses expected 401, got $AUTH_CODE"
fi

# ── Step 3: Check Codex CLI is available ──
echo ""
echo "── Step 3: Verify Codex CLI ──"
if command -v codex &> /dev/null; then
    CODEX_VER=$(codex --version 2>/dev/null || echo "unknown")
    log_pass "Codex CLI found: $CODEX_VER"
else
    echo -e "  ${YELLOW}⚠${NC} Codex CLI not found; skipping end-to-end query"
    echo "  Install: https://github.com/openai/codex"
    echo "  Then run: codex provider add frugalis http://$HOST/v1 --api-key $TOKEN"
    echo "  Then: codex 'what is 2+2?'"
fi

# ── Step 4: Run Codex CLI query (if available) ──
if command -v codex &> /dev/null; then
    echo ""
    echo "── Step 4: Run Codex CLI query through Frugalis ──"
    echo "  Provider URL: http://$HOST/v1"
    echo "  Token: $TOKEN"
    echo ""

    CODEX_OUTPUT=$(codex "what is 2+2?" 2>&1 || true)
    CODEX_EXIT=$?
    if [ "$CODEX_EXIT" = "0" ] && echo "$CODEX_OUTPUT" | grep -qi "4"; then
        log_pass "Codex CLI query succeeded through Frugalis"
    else
        log_fail "Codex CLI query failed (exit=$CODEX_EXIT); output: ${CODEX_OUTPUT:0:200}"
    fi
fi

# ── Results ──
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
printf "Results: ${GREEN}PASS=${PASS}${NC}  ${RED}FAIL=${FAIL}${NC}\n"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
if [ "$FAIL" -gt 0 ]; then exit 1; fi
