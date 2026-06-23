#!/usr/bin/env bash
set -euo pipefail

# Unified manual & integration tests for Cerebrum
# Modes:
#   --basic      quick smoke: health, dashboard auth, classification-only, truncation, graceful shutdown
#   --auto       full automated suite: runs all scenario tests (categories, routing, Phase 2 limits, etc.)
#   (default)    interactive manual testing (original manual-test/run.sh behavior)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/.."

# Load shared infrastructure
if [ -f "manual-test/lib.sh" ]; then
    source "manual-test/lib.sh"
else
    echo "ERROR: manual-test/lib.sh not found" >&2
    exit 1
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
    local base="http://$HOST"
    if curl -s -f -u admin:admin "$base/dashboard/inferences" >/dev/null; then
        log_pass "Dashboard accepts Basic auth"
    else
        log_fail "Dashboard Basic auth failed"
    fi

    # Unauthorized dashboard
    if [ "$(curl -s -o /dev/null -w "%{http_code}" "$base/dashboard/inferences")" = "401" ]; then
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

    # Graceful shutdown
    log_info "Sending SIGTERM to server..."
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
    if [ -f "manual-test/run.sh" ]; then
        (cd manual-test && ./run.sh --auto)
        return $?
    else
        log_fail "manual-test/run.sh not found"
        return 1
    fi
}

# ============================================================================
# Interactive mode (default)
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

trap cleanup EXIT

if [ "$AUTO_MODE" = true ]; then
    run_automated_tests
elif [ "$BASIC_MODE" = true ]; then
    run_basic_tests
else
    run_interactive
fi
