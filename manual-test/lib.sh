#!/bin/bash
# ============================================================================
# Shared test infrastructure for Cerebrum manual & automated integration tests.
# Source this file from test scripts: source "$(dirname "$0")/lib.sh"
# ============================================================================

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Configuration (overridable via env)
BINARY="./target/release/cerebrum"
HOST="${HOST:-127.0.0.1:10000}"
HEALTH_URL="http://$HOST/health"
CLASSIFY_URL="http://$HOST/v1/classify"
MESSAGES_URL="http://$HOST/v1/messages"
TOKEN="${PROXY_API_BEARER_TOKEN:-test-token-123}"

PASS=0
FAIL=0
SERVER_PID=""

log_info() {
    printf "${BLUE}[INFO]${NC} %s\n" "$1"
}

log_pass() {
    printf "${GREEN}[✓]${NC} %s\n" "$1"
    PASS=$((PASS+1))
}

log_fail() {
    printf "${RED}[✗]${NC} %s\n" "$1"
    FAIL=$((FAIL+1))
}

section() {
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    printf "${YELLOW}%s${NC}\n" "$1"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
}

build_server() {
    section "Building Server"
    if [ ! -f "$BINARY" ]; then
        log_info "Building release binary..."
        cargo build --release
        log_pass "Build complete"
    else
        log_info "Binary already exists, skipping build"
    fi
}

start_server() {
    local config_file="$1"
    local log_file="/tmp/cerebrum-test-$$.log"

    log_info "Starting server with config: ${config_file:-<none>}"

    export CONFIG_PATH="${config_file:-}"
    export RUST_LOG="info"
    export PROXY_API_BEARER_TOKEN="$TOKEN"
    export DASHBOARD_BASIC_USER="admin"
    export DASHBOARD_BASIC_PASSWORD="admin"
    export PORT="10000"

    "$BINARY" > "$log_file" 2>&1 &
    SERVER_PID=$!

    local attempts=30
    for i in $(seq 1 $attempts); do
        if curl -s -f "$HEALTH_URL" > /dev/null 2>&1; then
            log_pass "Server started (PID $SERVER_PID, health OK)"
            return 0
        fi
        printf "."
        sleep 1
    done

    log_fail "Server failed to start within $attempts seconds"
    echo "Server log:"
    tail -20 "$log_file" || true
    stop_server
    return 1
}

stop_server() {
    if [ -n "$SERVER_PID" ]; then
        log_info "Stopping server (PID $SERVER_PID)..."
        kill $SERVER_PID 2>/dev/null || true
        wait $SERVER_PID 2>/dev/null || true
        SERVER_PID=""
        log_pass "Server stopped"
    fi
}

cleanup() {
    stop_server
    rm -f /tmp/cerebrum-config-*.toml
    rm -f /tmp/cerebrum-config-*.yaml
    rm -rf /tmp/cerebrum-patterns
    if [ $FAIL -eq 0 ]; then
        rm -f /tmp/cerebrum-test-$$.log
    else
        echo "Server log preserved at: /tmp/cerebrum-test-$$.log" >&2
    fi
}

classify() {
    local prompt="$1"

    response=$(curl -s -w "\n%{http_code}" \
        "$CLASSIFY_URL" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"messages\":[{\"role\":\"user\",\"content\":\"$prompt\"}]}" 2>/dev/null) || return 1

    http_code=$(echo "$response" | tail -n1)
    body=$(echo "$response" | sed '$d')

    if [ "$http_code" != "200" ]; then
        echo "ERROR" >&2
        return 1
    fi

    category=$(echo "$body" | python3 -c "
import json,sys
try:
    d = json.load(sys.stdin)
    print(d.get('category', 'UNKNOWN'))
except:
    print('ERROR')
" 2>/dev/null || echo "ERROR")

    model=$(echo "$body" | python3 -c "
import json,sys
try:
    d = json.load(sys.stdin)
    print(d.get('model', ''))
except:
    print('')
" 2>/dev/null || echo "")

    printf "(category=%s, model=%s)\n" "$category" "$model" >&2

    echo "$category"
}

# Classify a prompt via the /v1/messages (Anthropic pass-through) endpoint.
# Sends a body shaped like the Anthropic Messages API. Returns the category
# from the response:
# - When no http_client is configured (or api_key_env is unset), the proxy
#   returns classification JSON with a `category` field.
# - When a real upstream is configured and reachable, the proxy forwards
#   and the response is the upstream's body (no `category` field). In that
#   case we infer the category from the routed model name (parity with
#   `test_hardcoded_defaults`'s model→category heuristic).
# In both cases, the function prints the resulting category on stdout and
# `(category=X, model=Y)` on stderr for debugging.
classify_messages() {
    local prompt="$1"

    response=$(curl -s -w "\n%{http_code}" \
        "$MESSAGES_URL" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"model\":\"claude-3.5\",\"max_tokens\":100,\"messages\":[{\"role\":\"user\",\"content\":\"$prompt\"}]}" 2>/dev/null) || return 1

    http_code=$(echo "$response" | tail -n1)
    body=$(echo "$response" | sed '$d')

    if [ "$http_code" != "200" ]; then
        echo "ERROR" >&2
        return 1
    fi

    # First try the classification-JSON shape (no upstream). If that fails
    # the proxy forwarded to a real upstream — infer from the routed model
    # the same way `test_hardcoded_defaults` does.
    category=$(echo "$body" | python3 -c "
import json,sys
try:
    d = json.load(sys.stdin)
    cat = d.get('category')
    if cat:
        print(cat)
    else:
        model = d.get('model', '')
        if '70b' in model or 'reading' in model or 'llama-3.1-70b' in model:
            print('FILE_READING')
        elif '3.3-70b' in model or 'complex' in model or 'llama-3.3-70b' in model:
            print('COMPLEX_REASONING')
        elif '8b' in model or 'nano' in model or 'llama-3.1-8b' in model:
            print('CASUAL')
        elif 'coder' in model or 'qwen' in model:
            print('SYNTAX_FIX')
        else:
            print('UNKNOWN_UPSTREAM')
except Exception:
    print('ERROR')
" 2>/dev/null || echo "ERROR")

    model=$(echo "$body" | python3 -c "
import json,sys
try:
    d = json.load(sys.stdin)
    print(d.get('model', ''))
except:
    print('')
" 2>/dev/null || echo "")

    printf "(category=%s, model=%s)\n" "$category" "$model" >&2

    echo "$category"
}
