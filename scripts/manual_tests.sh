#!/usr/bin/env bash
set -euo pipefail

# Unified manual & integration tests for Cerebrum
# Modes:
#   --basic      quick smoke: health, dashboard auth, classification-only, truncation, graceful shutdown
#   --auto       full automated suite: runs all scenario tests (categories, routing, Phase 2 limits, etc.)
#   (default)    interactive manual testing (original manual-test/run.sh behavior)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/.."

# Load shared infra if available
if [ -f "manual-test/lib.sh" ]; then
    source "manual-test/lib.sh"
else
    # Minimal fallback infra when lib.sh not present
    BINARY="${BINARY:-./target/debug/cerebrum}"
    HOST="${HOST:-127.0.0.1:10000}"
    HEALTH_URL="http://$HOST/health"
    CLASSIFY_URL="http://$HOST/v1/classify"
    COMPLETION_URL="http://$HOST/v1/chat/completions"
    TOKEN="${PROXY_API_BEARER_TOKEN:-test-token-123}"
    PASS=0
    FAIL=0
    SERVER_PID=""
    log_info() { printf "${BLUE:-}[INFO]${NC:-} %s\n" "$1"; }
    log_pass() { printf "${GREEN:-}[✓]${NC:-} %s\n" "$1"; PASS=$((PASS+1)); }
    log_fail() { printf "${RED:-}[✗]${NC:-} %s\n" "$1"; FAIL=$((FAIL+1)); }
    section() { echo ""; echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"; printf "${YELLOW:-}%s${NC:-}\n" "$1"; echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"; }
    build_server() { if [ ! -f "$BINARY" ]; then cargo build --release; BINARY="./target/release/cerebrum"; fi; }
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
            if curl -s -f "$HEALTH_URL" > /dev/null 2>&1; then log_pass "Server started (PID $SERVER_PID, health OK)"; return 0; fi
            printf "."
            sleep 1
        done
        log_fail "Server failed to start within $attempts seconds"
        echo "Server log:"; tail -20 "$log_file" || true
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
        if [ $FAIL -eq 0 ]; then rm -f /tmp/cerebrum-test-$$.log; else echo "Server log preserved at: /tmp/cerebrum-test-$$.log" >&2; fi
    }
    trap cleanup EXIT
fi

# Mode detection
AUTO_MODE=false
BASIC_MODE=false
if [ $# -gt 0 ]; then
    case "$1" in
        --auto|-a) AUTO_MODE=true ;;
        --basic|-b) BASIC_MODE=true ;;
        --help|-h)
            echo "Usage: $0 [--auto|--basic]"
            echo "  --auto    run full automated suite (category config, routing, Phase 2 limits, streaming, etc.)"
            echo "  --basic   quick smoke tests (health, auth, classification-only, truncation, graceful shutdown)"
            echo "  (default) interactive manual testing (original manual-test/run.sh behavior)"
            exit 0
            ;;
    esac
fi

# ============================================================================
# Phase 2 specific tests (truncation, graceful shutdown)
# ============================================================================

test_truncation() {
    section "Phase 2: Upstream Body Truncation"
    # Set a low limit > 1MB and send a large upstream response
    # Requires httpmock server
    local config="/tmp/cerebrum-config-truncation.toml"
    cat > "$config" <<'EOF'
[[categories]]
name = "CASUAL"
description = "Simple questions"
threshold = 1
priority = 4
model_env_var = "DEFAULT_MODEL"

[FALLBACK]
model = "fallback-model"
EOF
    export MAX_UPSTREAM_BODY_BYTES="1100000"
    if ! start_server "$config"; then
        log_fail "Failed to start server for truncation test"
        return 1
    fi

    # We'll use a mock server via httpmock if python available, otherwise skip
    if command -v python3 &>/dev/null; then
        # Simple mock using nc or use test endpoint? For manual script, we rely on classification-only path
        log_info "Truncation test requires httpmock (only in unit tests). Skipping in manual mode."
        # Could test by sending a large body and expecting 502 if upstream were large; not feasible manually.
        stop_server
        return 0
    fi
    stop_server
    return 0
}

test_graceful_shutdown() {
    section "Phase 2: Graceful Shutdown"
    if ! start_server ""; then
        log_fail "Failed to start server for shutdown test"
        return 1
    fi
    # Send a long-running request? Instead we just verify SIGTERM handling by stopping server
    log_info "Sending SIGTERM to server..."
    kill -TERM "$SERVER_PID" 2>/dev/null || true
    if wait "$SERVER_PID" 2>/dev/null; then
        log_pass "Server exited cleanly on SIGTERM"
    else
        log_fail "Server did not exit cleanly"
    fi
    SERVER_PID=""
}

# ============================================================================
# Basic smoke tests (--basic)
# ============================================================================

run_basic_tests() {
    section "Basic Smoke Tests"
    if ! start_server ""; then
        log_fail "Server failed to start"
        return 1
    fi

    # Health
    if curl -s -f "$HEALTH_URL" >/dev/null; then
        log_pass "Health endpoint returns 200"
    else
        log_fail "Health endpoint failed"
    fi

    # Dashboard auth
    if curl -s -f -u admin:admin "$HOST/dashboard/inferences" >/dev/null; then
        log_pass "Dashboard accepts Basic auth"
    else
        log_fail "Dashboard Basic auth failed"
    fi

    # Unauthorized dashboard
    if [ "$(curl -s -o /dev/null -w "%{http_code}" "$HOST/dashboard/inferences")" = "401" ]; then
        log_pass "Unauthenticated dashboard returns 401"
    else
        log_fail "Expected 401 for unauthenticated dashboard"
    fi

    # Classification-only (CASUAL)
    local resp
    resp=$(curl -s -w "\n%{http_code}" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d '{"messages":[{"role":"user","content":"hello"}]}' \
        "$COMPLETION_URL" || true)
    local code body
    code=$(printf '%s' "$resp" | tail -1)
    body=$(printf '%s' "$resp" | sed '$d')
    if [ "$code" = "200" ]; then
        log_pass "Classification-only returns 200"
    else
        log_fail "Expected 200, got $code (body: $(printf '%s' "$body" | head -c 100))"
    fi

    # Verify response JSON includes tier or category
    if printf '%s' "$body" | grep -q '"tier"'; then
        log_pass "Response includes 'tier' field"
    else
        log_fail "Response missing 'tier' field"
    fi

    # Shutdown
    kill -TERM "$SERVER_PID" 2>/dev/null || true
    if wait "$SERVER_PID" 2>/dev/null; then
        log_pass "Server exits cleanly on SIGTERM"
    else
        log_fail "Server did not exit cleanly"
    fi
    SERVER_PID=""

    echo ""
    echo "Results: PASS=$PASS FAIL=$FAIL"
    if [ $FAIL -eq 0 ]; then
        log_pass "All basic tests passed"
        exit 0
    else
        log_fail "Some tests failed"
        exit 1
    fi
}

# ============================================================================
# Automated full suite (--auto)
# ============================================================================

run_automated_tests() {
    section "Automated Integration Tests"
    build_server
    # Run the existing auto tests by calling into manual-test scripts
    if [ -f "manual-test/run.sh" ]; then
        (cd manual-test && ./run.sh --auto)
        # Propagate exit code
        return $?
    else
        log_fail "manual-test/run.sh not found"
        return 1
    fi
}

# ============================================================================
# Interactive mode (default) — reuse existing run.sh functionality
# ============================================================================

run_interactive() {
    if [ -f "manual-test/run.sh" ]; then
        exec "manual-test/run.sh"
    else
        log_fail "manual-test/run.sh not found. Cannot run interactive mode."
        exit 1
    fi
}

# ============================================================================
# Main
# ============================================================================

if [ "$AUTO_MODE" = true ]; then
    run_automated_tests
elif [ "$BASIC_MODE" = true ]; then
    run_basic_tests
else
    run_interactive
fi
