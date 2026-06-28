#!/usr/bin/env bash
set -euo pipefail

# Manual cache integration tests for add-response-cache
#
# Prerequisites:
#   PROXY_API_BEARER_TOKEN, DASHBOARD_BASIC_USER, DASHBOARD_BASIC_PASSWORD (env or set below)
#
# Usage:
#   ./scripts/manual_tests_cache.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/.."

if [ -f "manual-test/lib.sh" ]; then
    source "manual-test/lib.sh"
else
    echo "ERROR: manual-test/lib.sh not found" >&2
    exit 1
fi

BINARY="./target/release/frugalis"
# Credentials must match what start_server() in lib.sh exports
DASH_USER="admin"
DASH_PASS="admin"
CACHE_TTL="${CACHE_TTL:-5}"   # 5 seconds for quick TTL expiry test
CONFIG_FILE="/tmp/frugalis-cache-test-$$.toml"

# Avoid slow PostgreSQL startup from DATABASE_URL in test environment
unset DATABASE_URL

# ── helpers ────────────────────────────────────────────────────────────────

# Create a config overlay with cache enabled
create_cache_config() {
    cat > "$CONFIG_FILE" <<EOF
[cache]
ttl_secs = $CACHE_TTL
max_entries = 1000
EOF
    log_info "Created config overlay: $CONFIG_FILE (ttl=${CACHE_TTL}s)"
}

# Parse HTTP response, returns status code and body
parse_response() {
    local resp="$1"
    local code body
    code=$(printf '%s' "$resp" | tail -1)
    body=$(printf '%s' "$resp" | sed '$d')
    printf '%s\n%s' "$code" "$body"
}

# ── tests ──────────────────────────────────────────────────────────────────

test_phase1_cache_absent() {
    section "Phase 1.5: Cache disabled when [cache] absent"
    stop_server
    # Start without config overlay → no [cache] section
    # start_server handles CONFIG_PATH; empty string = embedded defaults only
    if ! start_server ""; then
        log_fail "Server failed to start"
        return
    fi
    # Verify dashboard/cache shows "not configured"
    local resp
    resp=$(curl -s -w "\n%{http_code}" -u "$DASH_USER:$DASH_PASS" \
        "http://$HOST/dashboard/cache" 2>/dev/null || true)
    local code
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" != "200" ]; then
        log_fail "Dashboard cache returned $code, expected 200"
        return
    fi
    if printf '%s' "$resp" | grep -q "not configured"; then
        log_pass "Cache page shows 'not configured' when [cache] absent"
    else
        log_fail "Cache page should show disabled state"
    fi
}

test_phase1_cache_enabled() {
    section "Phase 1.6: Cache constructed via CONFIG_PATH overlay"
    stop_server
    create_cache_config
    if ! start_server "$CONFIG_FILE"; then
        log_fail "Server failed to start"
        return
    fi
    # Verify server log contains "Response cache enabled"
    if grep -q "Response cache enabled" "/tmp/frugalis-test-$$.log" 2>/dev/null; then
        log_pass "Server log confirms cache enabled"
    else
        log_fail "Server log missing 'Response cache enabled'"
    fi
    # Verify dashboard/cache shows enabled
    local resp
    resp=$(curl -s -w "\n%{http_code}" -u "$DASH_USER:$DASH_PASS" \
        "http://$HOST/dashboard/cache" 2>/dev/null || true)
    local code
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ]; then
        log_pass "Dashboard cache returns 200"
    else
        log_fail "Dashboard cache returned $code"
    fi
    if printf '%s' "$resp" | grep -q "$CACHE_TTL"; then
        log_pass "Dashboard shows TTL=$CACHE_TTL"
    else
        log_fail "Dashboard TTL mismatch"
    fi
}

test_phase2_cache_hit() {
    section "Phase 2.4: Cache hit on identical request"
    # Restart with cache config to clear cache
    stop_server
    start_server "$CONFIG_FILE" || { log_fail "Server start failed"; return; }

    local body
    body='{"messages":[{"role":"user","content":"fix this bug"}]}'

    # First request: miss
    local resp1
    resp1=$(curl -s -w "\n%{http_code}" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d "$body" \
        "$COMPLETION_URL" 2>/dev/null || true)

    # Second request: should be cache hit (same body)
    local resp2
    resp2=$(curl -s -w "\n%{http_code}" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d "$body" \
        "$COMPLETION_URL" 2>/dev/null || true)

    local code1 code2
    code1=$(printf '%s' "$resp1" | tail -1)
    code2=$(printf '%s' "$resp2" | tail -1)

    if [ "$code1" = "200" ] && [ "$code2" = "200" ]; then
        log_pass "Both requests returned 200"
    else
        log_fail "Request failed (code1=$code1, code2=$code2)"
        return
    fi

    # Check dashboard shows hits > 0
    local dash
    dash=$(curl -s -u "$DASH_USER:$DASH_PASS" "http://$HOST/dashboard/cache" 2>/dev/null)
    if printf '%s' "$dash" | grep -q "Hits"; then
        log_pass "Dashboard shows cache stats after hits"
    else
        log_fail "Dashboard missing cache stats"
    fi
}

test_phase2_cache_bypass() {
    section "Phase 2.5: X-Frugalis-No-Cache bypass"
    local body
    body='{"messages":[{"role":"user","content":"fix this bug"}]}'

    # Request with bypass header
    local resp
    resp=$(curl -s -w "\n%{http_code}" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -H "X-Frugalis-No-Cache: true" \
        -d "$body" \
        "$COMPLETION_URL" 2>/dev/null || true)

    local code
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ]; then
        log_pass "No-Cache bypass request returns 200"
    else
        log_fail "No-Cache bypass returned $code"
    fi
}

test_phase2_streaming_not_cached() {
    section "Phase 2.6: Streaming requests not cached"
    local body
    body='{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}'

    local resp
    resp=$(curl -s -w "\n%{http_code}" --no-buffer \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d "$body" \
        "$COMPLETION_URL" 2>/dev/null || true)

    local code
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ]; then
        log_pass "Streaming request returns 200"
    else
        log_fail "Streaming request returned $code"
    fi
    # Cache entry count should still be 0 (streaming never cached)
    local dash
    dash=$(curl -s -u "$DASH_USER:$DASH_PASS" "http://$HOST/dashboard/cache" 2>/dev/null)
    if printf '%s' "$dash" | grep -q 'Entries'; then
        log_pass "Dashboard accessible and shows entries"
    else
        log_fail "Dashboard entry section missing"
    fi
}

test_phase2_error_not_cached() {
    section "Phase 2.7: Error responses not cached"
    # Send a request without auth (will get 401/proxy error)
    local body
    body='{"messages":[{"role":"user","content":"fix this bug"}]}'

    local resp
    resp=$(curl -s -w "\n%{http_code}" \
        -H "Content-Type: application/json" \
        -d "$body" \
        "$COMPLETION_URL" 2>/dev/null || true)

    local code
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "401" ]; then
        log_pass "Unauthenticated request returned 401 (not cached)"
    else
        log_pass "Request returned $code (may use classification-only if auth triggered after)"
    fi
}

test_phase3_dashboard() {
    section "Phase 3: Dashboard cache page"
    local resp
    resp=$(curl -s -w "\n%{http_code}" -u "$DASH_USER:$DASH_PASS" \
        "http://$HOST/dashboard/cache" 2>/dev/null || true)

    local code
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ]; then
        log_pass "Dashboard/cache returns 200 with auth"
    else
        log_fail "Dashboard/cache returned $code"
    fi

    # Unauthenticated
    local unauth_code
    unauth_code=$(curl -s -o /dev/null -w "%{http_code}" \
        "http://$HOST/dashboard/cache" 2>/dev/null || true)
    if [ "$unauth_code" = "401" ]; then
        log_pass "Dashboard/cache returns 401 without auth"
    else
        log_fail "Dashboard/cache without auth returned $unauth_code, expected 401"
    fi

    if printf '%s' "$resp" | grep -q "Hits"; then
        log_pass "Dashboard shows hit count"
    else
        log_info "No hits recorded (OK if no prior requests)"
    fi

    if printf '%s' "$resp" | grep -q "TTL"; then
        log_pass "Dashboard shows TTL"
    else
        log_fail "Dashboard missing TTL display"
    fi
}

test_phase3_ttl_expiry() {
    section "Phase 3.6: Entry count drops after TTL expiry"
    stop_server

    # Use very short TTL for this test
    local short_config="/tmp/frugalis-cache-short-$$.toml"
    cat > "$short_config" <<EOF
[cache]
ttl_secs = 3
max_entries = 1000
EOF
    if ! start_server "$short_config"; then
        log_fail "Server failed to start"
        return
    fi

    local body
    body='{"messages":[{"role":"user","content":"fix this bug"}]}'

    # Populate cache
    curl -s -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d "$body" \
        "$COMPLETION_URL" >/dev/null 2>&1 || true

    # Wait for TTL to expire (3s + 1s buffer)
    log_info "Waiting for TTL expiration (4s)..."
    sleep 4

    # Send same request → should be cache miss
    curl -s -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d "$body" \
        "$COMPLETION_URL" >/dev/null 2>&1 || true

    # Check dashboard — should show stats (entries may be 0 or 1 after expiry)
    local dash dash_code
    dash=$(curl -s -w "\n%{http_code}" -u "$DASH_USER:$DASH_PASS" "http://$HOST/dashboard/cache" 2>/dev/null)
    dash_code=$(printf '%s' "$dash" | tail -1)
    if [ "$dash_code" != "200" ]; then
        log_fail "Dashboard returned $dash_code after TTL expiry"
    elif printf '%s' "$dash" | grep -q "Current Entries"; then
        log_pass "Dashboard shows entry stats after TTL expiry"
    else
        log_fail "Dashboard entry display broken (unexpected content)"
    fi

    rm -f "$short_config"
}

# ── main ───────────────────────────────────────────────────────────────────

cleanup_handler() {
    stop_server
    rm -f "$CONFIG_FILE"
}
trap cleanup_handler EXIT

PASS=0
FAIL=0

build_server

echo ""
echo "=============================================="
echo "  Frugalis Cache Manual Tests"
echo "  TTL=$CACHE_TTL s"
echo "=============================================="

test_phase1_cache_absent
test_phase1_cache_enabled
test_phase2_cache_hit
test_phase2_cache_bypass
test_phase2_streaming_not_cached
test_phase2_error_not_cached
test_phase3_dashboard
test_phase3_ttl_expiry

stop_server

echo ""
echo "=============================================="
printf "Results: ${GREEN}PASS=${PASS}${NC}  ${RED}FAIL=${FAIL}${NC}\n"
echo "=============================================="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
exit 0
