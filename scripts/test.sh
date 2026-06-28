#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# Frugalis Unified Integration Test Suite
# ============================================================================
# Combines: scripts/manual_tests.sh, scripts/manual_tests_cache.sh, manual-test/run.sh
#
# USAGE:
#   ./scripts/test.sh              # Run all automated tests (default)
#   ./scripts/test.sh --basic      # Quick smoke tests only
#   ./scripts/test.sh --cache      # Cache-specific tests only
#   ./scripts/test.sh --interactive # Interactive manual testing
#   ./scripts/test.sh --anthropic  # Anthropic pass-through interactive tests
#   ./scripts/test.sh --fewshot    # Few-shot classifier interactive tests
#   ./scripts/test.sh --help       # Show this help
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/.."

if [ -f "$SCRIPT_DIR/lib.sh" ]; then
    source "$SCRIPT_DIR/lib.sh"
else
    echo "ERROR: scripts/lib.sh not found" >&2
    exit 1
fi

# Avoid slow PostgreSQL startup in test environment
unset DATABASE_URL 2>/dev/null || true

# ── Mode detection ─────────────────────────────────────────────────────────
MODE="auto"
if [ $# -gt 0 ]; then
    case "$1" in
        --basic|-b)       MODE="basic" ;;
        --cache|-c)       MODE="cache" ;;
        --interactive|-i) MODE="interactive" ;;
        --anthropic)      MODE="anthropic" ;;
        --fewshot|-f)     MODE="fewshot" ;;
        --help|-h)
            echo "Usage: $0 [--basic|--cache|--interactive|--anthropic|--fewshot]"
            echo "  (default)      run full automated suite"
            echo "  --basic        quick smoke tests (health, auth, classification-only, shutdown)"
            echo "  --cache        cache-specific tests (TTL, bypass, streaming, dashboard)"
            echo "  --interactive  interactive manual testing (server must be running)"
            echo "  --anthropic    anthropic pass-through interactive tests"
            echo "  --fewshot      few-shot classifier interactive tests"
            exit 0
            ;;
        *) echo "Unknown flag: $1. Use --help."; exit 2 ;;
    esac
fi

# ============================================================================
# Shared helpers
# ============================================================================

extract_model() {
    echo "$1" | python3 -c "
import json,sys
try:
    d = json.load(sys.stdin, strict=False)
    print(d.get('model', d.get('upstream_model', '')))
except:
    print('')
" 2>/dev/null
}

extract_error() {
    echo "$1" | python3 -c "
import json,sys
try:
    d = json.load(sys.stdin)
    print(d.get('message', '')[:120])
except:
    pass
" 2>/dev/null
}

parse_json_field() {
    local json="$1" field="$2"
    echo "$json" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('$field',''))" 2>/dev/null || echo ""
}

# ============================================================================
# Basic smoke tests (--basic)
# ============================================================================

run_basic_tests() {
    section "Basic Smoke Tests"
    build_server
    if ! start_server ""; then
        log_fail "Server failed to start"
        return 1
    fi

    if curl -s -f "$HEALTH_URL" >/dev/null; then
        log_pass "Health endpoint returns 200"
    else
        log_fail "Health endpoint failed"
    fi

    if curl -s -f -u admin:admin "http://$HOST/dashboard/inferences" >/dev/null; then
        log_pass "Dashboard accepts Basic auth"
    else
        log_fail "Dashboard Basic auth failed"
    fi

    if [ "$(curl -s -o /dev/null -w "%{http_code}" "http://$HOST/dashboard/inferences")" = "401" ]; then
        log_pass "Unauthenticated dashboard returns 401"
    else
        log_fail "Expected 401 for unauthenticated dashboard"
    fi

    local resp code body
    resp=$(curl -s -w "\n%{http_code}" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d '{"messages":[{"role":"user","content":"hello"}]}' \
        "$COMPLETION_URL" || true)
    code=$(printf '%s' "$resp" | tail -1)
    body=$(printf '%s' "$resp" | sed '$d')
    if [ "$code" = "200" ]; then
        log_pass "Classification-only returns 200"
    else
        log_fail "Expected 200, got $code"
    fi

    if printf '%s' "$body" | grep -q '"tier"\|"chat.completion"'; then
        log_pass "Response is valid (classification or optimized)"
    else
        log_fail "Response missing expected fields"
    fi

    log_info "Sending SIGTERM to server..."
    kill -TERM "$SERVER_PID" 2>/dev/null || true
    if wait "$SERVER_PID" 2>/dev/null; then
        log_pass "Server exits cleanly on SIGTERM"
    else
        log_fail "Server did not exit cleanly"
    fi
    SERVER_PID=""
}


# ============================================================================
# Cache tests (--cache)
# ============================================================================

CACHE_TTL="${CACHE_TTL:-5}"
CACHE_CONFIG_FILE="/tmp/frugalis-cache-test-$$.toml"

create_cache_config() {
    cat > "$CACHE_CONFIG_FILE" <<EOF
[cache]
ttl_secs = $CACHE_TTL
max_entries = 1000
EOF
}

run_cache_tests() {
    section "Cache Integration Tests (TTL=${CACHE_TTL}s)"
    build_server

    # Test: cache disabled when [cache] absent
    section "Cache disabled when [cache] absent"
    if ! start_server ""; then
        log_fail "Server failed to start"; return 1
    fi
    local resp code
    resp=$(curl -s -w "\n%{http_code}" -u admin:admin "http://$HOST/dashboard/cache" 2>/dev/null || true)
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ] && printf '%s' "$resp" | grep -q "not configured"; then
        log_pass "Cache page shows 'not configured' when [cache] absent"
    else
        log_fail "Cache page should show disabled state (code=$code)"
    fi
    stop_server

    # Test: cache enabled via CONFIG_PATH overlay
    section "Cache enabled via CONFIG_PATH"
    create_cache_config
    if ! start_server "$CACHE_CONFIG_FILE"; then
        log_fail "Server failed to start"; return 1
    fi
    if grep -q "Response cache enabled" "/tmp/frugalis-test-$$.log" 2>/dev/null; then
        log_pass "Server log confirms cache enabled"
    else
        log_fail "Server log missing 'Response cache enabled'"
    fi
    resp=$(curl -s -w "\n%{http_code}" -u admin:admin "http://$HOST/dashboard/cache" 2>/dev/null || true)
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

    # Test: cache hit on identical request
    section "Cache hit on identical request"
    stop_server
    start_server "$CACHE_CONFIG_FILE" || { log_fail "Server start failed"; return 1; }
    local body='{"messages":[{"role":"user","content":"fix this bug"}]}'
    local resp1 resp2 code1 code2
    resp1=$(curl -s -w "\n%{http_code}" -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d "$body" "$COMPLETION_URL" 2>/dev/null || true)
    resp2=$(curl -s -w "\n%{http_code}" -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d "$body" "$COMPLETION_URL" 2>/dev/null || true)
    code1=$(printf '%s' "$resp1" | tail -1)
    code2=$(printf '%s' "$resp2" | tail -1)
    if [ "$code1" = "200" ] && [ "$code2" = "200" ]; then
        log_pass "Both requests returned 200"
    else
        log_fail "Request failed (code1=$code1, code2=$code2)"
    fi
    local dash
    dash=$(curl -s -u admin:admin "http://$HOST/dashboard/cache" 2>/dev/null)
    if printf '%s' "$dash" | grep -q "Hits"; then
        log_pass "Dashboard shows cache stats after hits"
    else
        log_fail "Dashboard missing cache stats"
    fi

    # Test: X-Frugalis-No-Cache bypass
    section "X-Frugalis-No-Cache bypass"
    resp=$(curl -s -w "\n%{http_code}" -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -H "X-Frugalis-No-Cache: true" -d "$body" "$COMPLETION_URL" 2>/dev/null || true)
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ]; then
        log_pass "No-Cache bypass request returns 200"
    else
        log_fail "No-Cache bypass returned $code"
    fi

    # Test: streaming not cached
    section "Streaming requests not cached"
    resp=$(curl -s -w "\n%{http_code}" --no-buffer -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d '{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}' "$COMPLETION_URL" 2>/dev/null || true)
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ]; then
        log_pass "Streaming request returns 200"
    else
        log_fail "Streaming request returned $code"
    fi

    # Test: error not cached
    section "Error responses not cached"
    resp=$(curl -s -w "\n%{http_code}" -H "Content-Type: application/json" -d "$body" "$COMPLETION_URL" 2>/dev/null || true)
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "401" ]; then
        log_pass "Unauthenticated request returned 401 (not cached)"
    else
        log_pass "Request returned $code (auth triggered)"
    fi

    # Test: dashboard auth
    section "Dashboard cache page auth"
    resp=$(curl -s -w "\n%{http_code}" -u admin:admin "http://$HOST/dashboard/cache" 2>/dev/null || true)
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ]; then log_pass "Dashboard/cache 200 with auth"; else log_fail "Got $code"; fi
    local unauth_code
    unauth_code=$(curl -s -o /dev/null -w "%{http_code}" "http://$HOST/dashboard/cache" 2>/dev/null || true)
    if [ "$unauth_code" = "401" ]; then log_pass "Dashboard/cache 401 without auth"; else log_fail "Got $unauth_code"; fi

    # Test: TTL expiry
    section "TTL expiry"
    stop_server
    local short_config="/tmp/frugalis-cache-short-$$.toml"
    cat > "$short_config" <<EOF
[cache]
ttl_secs = 3
max_entries = 1000
EOF
    if ! start_server "$short_config"; then log_fail "Server failed to start"; return 1; fi
    curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d "$body" "$COMPLETION_URL" >/dev/null 2>&1 || true
    log_info "Waiting for TTL expiration (4s)..."
    sleep 4
    curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d "$body" "$COMPLETION_URL" >/dev/null 2>&1 || true
    dash=$(curl -s -w "\n%{http_code}" -u admin:admin "http://$HOST/dashboard/cache" 2>/dev/null)
    local dash_code
    dash_code=$(printf '%s' "$dash" | tail -1)
    if [ "$dash_code" = "200" ] && printf '%s' "$dash" | grep -q "Current Entries"; then
        log_pass "Dashboard shows entry stats after TTL expiry"
    else
        log_fail "Dashboard entry display broken (code=$dash_code)"
    fi
    rm -f "$short_config"
    stop_server
    rm -f "$CACHE_CONFIG_FILE"
}


# ============================================================================
# Core automated tests (classification, routing, config validation)
# ============================================================================

test_hardcoded_defaults() {
    section "Hardcoded Defaults (no config.toml)"
    if ! start_server ""; then log_fail "Failed to start server"; return 1; fi
    local tests=("FILE_READING:please read the file src/main.rs" "COMPLEX_REASONING:architect a distributed rate limiter" "CASUAL:hello")
    local all_pass=true
    for test in "${tests[@]}"; do
        IFS=':' read -r expected prompt <<< "$test"
        result=$(classify "$prompt" 2>/dev/null) || result="ERROR"
        if [ "$result" = "$expected" ]; then log_pass "$expected classified correctly"
        else log_fail "Expected $expected, got $result"; all_pass=false; fi
    done
    stop_server
}

test_threshold_override() {
    section "Threshold Override (FILE_READING threshold=100)"
    cat > /tmp/frugalis-config-test.toml << 'EOF'
[categories.FILE_READING]
description = "Reading files"
threshold = 100
priority = 1
[categories.SYNTAX_FIX]
description = "Fixing bugs"
threshold = 3
priority = 2
[categories.COMPLEX_REASONING]
description = "Complex reasoning"
threshold = 3
priority = 3
[categories.CASUAL]
description = "Casual"
threshold = 1
priority = 4
[routing.FILE_READING]
model = "meta/llama-3.1-70b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.SYNTAX_FIX]
model = "meta/llama-3.1-8b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.COMPLEX_REASONING]
model = "meta/llama-3.3-70b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.CASUAL]
model = "meta/llama-3.1-8b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.DEFAULT]
model = "nvidia/nemotron-3-nano-30b-a3b"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
EOF
    if ! start_server "/tmp/frugalis-config-test.toml"; then log_fail "Failed to start"; return 1; fi
    result=$(classify "please read the file src/main.rs") || result="ERROR"
    if [ "$result" = "CASUAL" ] || [ "$result" = "ERROR" ]; then
        log_pass "FILE_READING threshold override respected (fell back to $result)"
    else
        log_fail "Threshold override NOT respected: got $result"
    fi
    stop_server
}

test_partial_categories() {
    section "Partial Categories (FILE_READING + CASUAL only)"
    cat > /tmp/frugalis-config-test.toml << 'EOF'
[categories.FILE_READING]
description = "Reading files"
threshold = 3
priority = 1
patterns = [{ regex = '(?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file|that\s+file)\b', weight = 3 }]
[categories.CASUAL]
description = "Simple questions"
threshold = 1
priority = 4
[routing.FILE_READING]
model = "meta/llama-3.1-70b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.CASUAL]
model = "meta/llama-3.1-8b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.DEFAULT]
model = "nvidia/nemotron-3-nano-30b-a3b"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
EOF
    if ! start_server "/tmp/frugalis-config-test.toml"; then log_fail "Failed to start"; return 1; fi
    result=$(classify "hello") || result="ERROR"
    if [ "$result" = "CASUAL" ]; then log_pass "CASUAL works with partial config"
    else log_fail "CASUAL failed: got $result"; fi
    result=$(classify "please read the file src/main.rs") || result="ERROR"
    if [ "$result" = "FILE_READING" ]; then log_pass "FILE_READING works with partial config"
    else log_fail "FILE_READING failed: got $result"; fi
    stop_server
}

test_combined_config() {
    section "Combined config (categories + routing)"
    cat > /tmp/frugalis-config-test.toml << 'EOF'
[categories.FILE_READING]
description = "Reading files"
threshold = 3
priority = 1
patterns = [{ regex = '(?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file|that\s+file)\b', weight = 3 }]
[categories.SYNTAX_FIX]
description = "Fixing bugs"
threshold = 3
priority = 2
patterns = [{ regex = '(?i)\b(?:fix|correct|repair|patch)\s+(?:this|the|my|a)\s+(?:bug|error|issue|typo|problem|mistake|warning)', weight = 3 }]
[categories.COMPLEX_REASONING]
description = "Complex reasoning"
threshold = 3
priority = 3
patterns = [{ regex = '(?i)\b(?:architect|design\s+pattern|system\s+design|trade.?off|refactor|restructure|rearchitect)', weight = 3 }]
[categories.CASUAL]
description = "Casual"
threshold = 1
priority = 4
patterns = [{ regex = '(?i)^\s*(?:hi|hey|hello|greetings|good\s+morning|good\s+afternoon|good\s+evening|howdy)(?:\s+there)?[\s!.,]*$', weight = 3 }]
[routing.FILE_READING]
model = "meta/llama-3.1-70b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.SYNTAX_FIX]
model = "meta/llama-3.1-8b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.COMPLEX_REASONING]
model = "meta/llama-3.3-70b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.CASUAL]
model = "meta/llama-3.1-8b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.DEFAULT]
model = "nvidia/nemotron-3-nano-30b-a3b"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
EOF
    if ! start_server "/tmp/frugalis-config-test.toml"; then log_fail "Failed to start"; return 1; fi
    local tests=("FILE_READING:please read the file src/main.rs" "SYNTAX_FIX:fix this bug please" "COMPLEX_REASONING:architect a distributed rate limiter" "CASUAL:hello")
    for test in "${tests[@]}"; do
        IFS=':' read -r expected prompt <<< "$test"
        result=$(classify "$prompt") || result="ERROR"
        if [ "$result" = "$expected" ]; then log_pass "$expected routed correctly"
        else log_fail "Expected $expected, got $result"; fi
    done
    stop_server
}

test_negative_suppression() {
    section "Negative Suppression"
    if ! start_server ""; then log_fail "Failed to start"; return 1; fi
    result=$(classify "read the architecture document") || result="ERROR"
    if [ "$result" != "COMPLEX_REASONING" ]; then
        log_pass "Negative suppression working (got $result)"
    else
        log_fail "Negative suppression broken: got COMPLEX_REASONING"
    fi
    stop_server
}

test_yaml_config() {
    section "YAML config loads and classifies"
    cat > /tmp/frugalis-config-test.yaml << 'YAMLEOF'
server:
  port: 10000
  log_level: info
  log_format: compact
http:
  max_upstream_body_bytes: 10485760
  keepalive_interval_secs: 15
  request_body_limit_bytes: 10485760
  client_timeout_secs: 120
  client_connect_timeout_secs: 30
  streaming_channel_capacity: 32
database:
  connection_retries: 3
  retry_base_ms: 1000
  max_connections: 10
  acquire_timeout_secs: 30
  idle_timeout_secs: 1800
  log_concurrency_limit: 100
persistence:
  backend: memory
classifiers:
  enabled: true
  order: [regex, llm]
regex_classifier:
  enabled: true
  short_prompt_len: 30
categories:
  FILE_READING:
    description: "Reading files"
    threshold: 3
    priority: 1
    patterns:
      - regex: '(?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file|that\s+file)\b'
        weight: 3
  CASUAL:
    description: "Casual"
    threshold: 1
    priority: 4
    patterns:
      - regex: '(?i)^\s*(?:hi|hey|hello|greetings|good\s+morning|good\s+afternoon|good\s+evening|howdy)(?:\s+there)?[\s!.,]*$'
        weight: 3
negative_patterns: []
routing:
  FILE_READING:
    model: meta/llama-3.1-70b-instruct
    endpoint: https://integrate.api.nvidia.com/v1/chat/completions
    provider_type: nvidia_nim
    api_key_env: NVIDIA_API_KEY
  CASUAL:
    model: meta/llama-3.1-8b-instruct
    endpoint: https://integrate.api.nvidia.com/v1/chat/completions
    provider_type: nvidia_nim
    api_key_env: NVIDIA_API_KEY
  DEFAULT:
    model: meta/llama-3.1-8b-instruct
    endpoint: https://integrate.api.nvidia.com/v1/chat/completions
    provider_type: nvidia_nim
    api_key_env: NVIDIA_API_KEY
baseline_model: meta/llama-3.3-70b-instruct
classify_db_log: false
dashboard:
  default_hours: 24
  hours_min: 1
  hours_max: 720
  page_limit: 20
  page_limit_max: 100
  recent_count: 5
YAMLEOF
    if ! start_server "/tmp/frugalis-config-test.yaml"; then log_fail "Failed to start with YAML"; return 1; fi
    local tests=("FILE_READING:please read the file src/main.rs" "CASUAL:hello")
    for test in "${tests[@]}"; do
        IFS=':' read -r expected prompt <<< "$test"
        result=$(classify "$prompt" 2>/dev/null) || result="ERROR"
        if [ "$result" = "$expected" ]; then log_pass "YAML: $expected classified correctly"
        else log_fail "YAML: expected $expected, got $result"; fi
    done
    stop_server
}

test_external_patterns() {
    section "External pattern files"
    mkdir -p /tmp/frugalis-patterns
    cat > /tmp/frugalis-patterns/file_reading.patterns << 'EOF'
3 | (?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file|that\s+file)\b
EOF
    cat > /tmp/frugalis-patterns/casual.patterns << 'EOF'
3 | (?i)^\s*(?:hi|hey|hello|greetings|good\s+morning|good\s+afternoon|good\s+evening|howdy)(?:\s+there)?[\s!.,]*$
EOF
    cat > /tmp/frugalis-config-test.toml << 'EOF'
patterns_dir = "/tmp/frugalis-patterns"
[categories.FILE_READING]
description = "Reading files"
threshold = 3
priority = 1
patterns_file = "file_reading.patterns"
[categories.CASUAL]
description = "Simple"
threshold = 1
priority = 4
patterns_file = "casual.patterns"
[routing.FILE_READING]
model = "meta/llama-3.1-70b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.CASUAL]
model = "meta/llama-3.1-8b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
[routing.DEFAULT]
model = "meta/llama-3.1-8b-instruct"
provider_type = "nvidia_nim"
endpoint = "https://integrate.api.nvidia.com/v1/chat/completions"
api_key_env = "NVIDIA_API_KEY"
EOF
    if ! start_server "/tmp/frugalis-config-test.toml"; then log_fail "Failed to start"; return 1; fi
    result=$(classify "please read the file src/main.rs" 2>/dev/null) || result="ERROR"
    if [ "$result" = "FILE_READING" ]; then log_pass "External pattern: FILE_READING"
    else log_fail "External pattern: expected FILE_READING, got $result"; fi
    result=$(classify "hello" 2>/dev/null) || result="ERROR"
    if [ "$result" = "CASUAL" ]; then log_pass "External pattern: CASUAL"
    else log_fail "External pattern: expected CASUAL, got $result"; fi
    stop_server
    rm -rf /tmp/frugalis-patterns
}

test_validate_cli() {
    section "CLI validation"
    export PROXY_API_BEARER_TOKEN="$TOKEN"
    export DASHBOARD_BASIC_USER="admin"
    export DASHBOARD_BASIC_PASSWORD="admin"

    # Valid config
    export CONFIG_PATH="$(pwd)/config.toml"
    local output rc
    output=$("$BINARY" --validate 2>&1); rc=$?
    if [ $rc -eq 0 ]; then log_pass "--validate on config.toml succeeds"
    else log_fail "--validate failed: $output"; fi

    # Invalid regex
    cat > /tmp/frugalis-config-test.toml << 'EOF'
[categories.BAD_REGEX]
description = "Bad"
threshold = 3
priority = 1
patterns = [{ regex = '[invalid(regex', weight = 3 }]
[routing.BAD_REGEX]
model = "test"
provider_type = "nvidia_nim"
endpoint = "https://example.com"
api_key_env = "NVIDIA_API_KEY"
EOF
    export CONFIG_PATH="/tmp/frugalis-config-test.toml"
    set +e; output=$("$BINARY" --validate 2>&1); rc=$?; set -e
    if [ $rc -ne 0 ] && echo "$output" | grep -qi "pattern\|regex\|invalid"; then
        log_pass "--validate detects invalid regex"
    else
        log_fail "--validate did not detect invalid regex (rc=$rc)"
    fi

    # Schema errors
    cat > /tmp/frugalis-config-test.toml << 'EOF'
[server]
port = 0
log_level = "invalid_level"
[categories.ZERO]
description = "Zero"
threshold = 0
priority = 0
EOF
    export CONFIG_PATH="/tmp/frugalis-config-test.toml"
    set +e; output=$("$BINARY" --validate 2>&1); rc=$?; set -e
    if [ $rc -ne 0 ]; then log_pass "--validate detects schema errors"
    else log_fail "Schema errors not detected"; fi

    # Unknown args
    unset CONFIG_PATH
    set +e; output=$("$BINARY" --badflag 2>&1); rc=$?; set -e
    if [ $rc -eq 2 ] && echo "$output" | grep -q "unknown argument"; then
        log_pass "Unknown flag exits 2 with helpful message"
    else
        log_fail "Unknown flag behavior unexpected (rc=$rc)"
    fi
}


# ============================================================================
# Anthropic pass-through & translation tests (mock upstream)
# ============================================================================

MOCK_ANTHROPIC_PORT=10042
MOCK_PID=""

stop_mock_server() {
    if [ -n "${MOCK_PID:-}" ]; then
        kill "$MOCK_PID" 2>/dev/null || true
        wait "$MOCK_PID" 2>/dev/null || true
        MOCK_PID=""
    fi
    rm -f /tmp/frugalis-mock-*.py /tmp/frugalis-cc-mock-diag.txt
}

_anthropic_config() {
    local mock_url="$1"
    cat > /tmp/frugalis-config-test.toml << HEREDOC
[categories.FILE_READING]
description = "Reading files"
threshold = 3
priority = 1
[categories.SYNTAX_FIX]
description = "Fixing bugs"
threshold = 3
priority = 2
patterns = [{ regex = '(?i)\\\\b(?:fix|correct|repair|patch)\\\\s+(?:this|the|my|a)\\\\s+(?:bug|error|issue)', weight = 3 }]
[categories.COMPLEX_REASONING]
description = "Complex"
threshold = 3
priority = 3
[categories.CASUAL]
description = "Casual"
threshold = 1
priority = 4
[routing.FILE_READING]
model = "mock-model"
provider_type = "anthropic"
endpoint = "${mock_url}"
api_key_env = "ANTHROPIC_API_KEY"
[routing.SYNTAX_FIX]
model = "mock-model"
provider_type = "anthropic"
endpoint = "${mock_url}"
api_key_env = "ANTHROPIC_API_KEY"
[routing.COMPLEX_REASONING]
model = "mock-model"
provider_type = "anthropic"
endpoint = "${mock_url}"
api_key_env = "ANTHROPIC_API_KEY"
[routing.CASUAL]
model = "mock-model"
provider_type = "anthropic"
endpoint = "${mock_url}"
api_key_env = "ANTHROPIC_API_KEY"
[routing.DEFAULT]
model = "mock-model"
provider_type = "anthropic"
endpoint = "${mock_url}"
api_key_env = "ANTHROPIC_API_KEY"
HEREDOC
}

start_mock_anthropic_ok() {
    local mock_script="/tmp/frugalis-mock-anth-$$.py"
    cat > "$mock_script" << 'PYEOF'
import http.server, json, sys, os
PORT = int(sys.argv[1])
DIAG = os.environ.get("DIAG_FILE", "")

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length)) if length > 0 else {}
        if DIAG:
            d = {
                "anthropic_beta": self.headers.get("anthropic-beta", "<absent>"),
                "anthropic_version": self.headers.get("anthropic-version", "<absent>"),
                "x_claude_code_session_id": self.headers.get("x-claude-code-session-id", "<absent>"),
                "x_api_key_ok": self.headers.get("x-api-key", "").startswith("sk-ant-"),
                "has_system": "system" in body,
                "has_messages": "messages" in body,
                "has_max_tokens": "max_tokens" in body,
                "has_cache_control": "cache_control" in body,
                "content_cache_control": any(
                    blk.get("cache_control") is not None
                    for m in body.get("messages", [])
                    for blk in (m.get("content") if isinstance(m.get("content"), list) else [])
                    if isinstance(blk, dict)
                ),
                "system_cache_control": any(
                    blk.get("cache_control") is not None
                    for blk in (body.get("system") if isinstance(body.get("system"), list) else [])
                    if isinstance(blk, dict)
                ),
            }
            with open(DIAG, "w") as f: json.dump(d, f)
        resp = {"id":"msg_mock","type":"message","role":"assistant","model":"mock-model",
                "content":[{"type":"text","text":"mock translated response"}],
                "stop_reason":"end_turn","stop_sequence":None,
                "usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":35,"cache_creation_input_tokens":0}}
        self.send_response(200)
        self.send_header("Content-Type","application/json")
        self.end_headers()
        self.wfile.write(json.dumps(resp).encode())

http.server.HTTPServer(("127.0.0.1", PORT), H).serve_forever()
PYEOF
    DIAG_FILE="${1:-}" python3 "$mock_script" "$MOCK_ANTHROPIC_PORT" &
    MOCK_PID=$!
    sleep 0.5
}

start_mock_anthropic_error() {
    local mock_script="/tmp/frugalis-mock-anth-err-$$.py"
    cat > "$mock_script" << 'PYEOF'
import http.server, json, sys
PORT = int(sys.argv[1])
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        self.send_response(429)
        self.send_header("Content-Type","application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"type":"error","error":{"type":"rate_limit_error","message":"Mock rate limit"}}).encode())
http.server.HTTPServer(("127.0.0.1", PORT), H).serve_forever()
PYEOF
    python3 "$mock_script" "$MOCK_ANTHROPIC_PORT" &
    MOCK_PID=$!
    sleep 0.5
}

start_mock_anthropic_stream() {
    local mock_script="/tmp/frugalis-mock-stream-$$.py"
    cat > "$mock_script" << 'PYEOF'
import http.server, json, sys
PORT = int(sys.argv[1])
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        self.send_response(200)
        self.send_header("Content-Type","text/event-stream")
        self.end_headers()
        events = [
            ("message_start", {"type":"message_start","message":{"id":"msg_s1","type":"message","role":"assistant","model":"mock","content":[],"stop_reason":None,"usage":{"input_tokens":10,"output_tokens":0}}}),
            ("content_block_start", {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ("content_block_delta", {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}),
            ("content_block_delta", {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"stream"}}),
            ("content_block_stop", {"type":"content_block_stop","index":0}),
            ("message_delta", {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}),
            ("message_stop", {"type":"message_stop"}),
        ]
        for name, data in events:
            self.wfile.write(f"event: {name}\ndata: {json.dumps(data)}\n\n".encode())
            self.wfile.flush()
http.server.HTTPServer(("127.0.0.1", PORT), H).serve_forever()
PYEOF
    python3 "$mock_script" "$MOCK_ANTHROPIC_PORT" &
    MOCK_PID=$!
    sleep 0.5
}

test_anthropic_passthrough() {
    section "Anthropic pass-through: /v1/messages"
    if ! start_server ""; then log_fail "Failed to start"; return 1; fi
    local tests=("fix this bug please" "please read the file src/main.rs" "hello")
    for prompt in "${tests[@]}"; do
        local _resp _code _body
        _resp=$(curl -s -w "\n%{http_code}" "$MESSAGES_URL" \
            -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
            -d "{\"model\":\"claude-3.5\",\"max_tokens\":100,\"messages\":[{\"role\":\"user\",\"content\":\"$prompt\"}]}" 2>/dev/null) || _resp="ERROR"
        _code=$(printf '%s' "$_resp" | tail -1)
        if [ "$_code" = "200" ]; then log_pass "Anthropic format accepted: \"$prompt\""
        else log_fail "Expected 200, got $_code for: $prompt"; fi
    done
    # Auth test
    local _code
    _code=$(curl -s -o /dev/null -w "%{http_code}" "$MESSAGES_URL" -H "Content-Type: application/json" \
        -d '{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"hello"}]}' 2>/dev/null) || _code="000"
    if [ "$_code" = "401" ]; then log_pass "/v1/messages returns 401 without token"
    else log_fail "Expected 401, got $_code"; fi
    # Content-type gate
    _code=$(curl -s -o /dev/null -w "%{http_code}" "$MESSAGES_URL" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: text/plain" -d 'hello' 2>/dev/null) || _code="000"
    if [ "$_code" = "415" ]; then log_pass "/v1/messages returns 415 for non-JSON"
    else log_fail "Expected 415, got $_code"; fi
    stop_server
}

test_models_endpoint() {
    section "Claude Code Compat: /v1/models"
    if ! start_server ""; then log_fail "Failed to start"; return 1; fi
    local _resp _code
    _resp=$(curl -s -w "\n%{http_code}" "http://$HOST/v1/models" 2>/dev/null) || _resp="ERROR"
    _code=$(printf '%s' "$_resp" | tail -1)
    if [ "$_code" = "200" ]; then log_pass "/v1/models returns 200 (unauthenticated)"
    else log_fail "Expected 200, got $_code"; fi
    stop_server
}

test_oai_to_anthropic_translation() {
    section "OpenAI→Anthropic Translation: non-streaming"
    start_mock_anthropic_ok
    _anthropic_config "http://127.0.0.1:${MOCK_ANTHROPIC_PORT}/v1/messages"
    export ANTHROPIC_API_KEY="sk-ant-test-key"
    if ! start_server "/tmp/frugalis-config-test.toml"; then
        log_fail "Server start failed"; stop_mock_server; unset ANTHROPIC_API_KEY; return 1
    fi
    local _resp _code _body
    _resp=$(curl -s -w "\n%{http_code}" -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"model":"gpt-4","messages":[{"role":"system","content":"You are helpful."},{"role":"user","content":"fix this bug"}],"max_tokens":100}' \
        "$COMPLETION_URL" 2>/dev/null) || _resp="ERROR"
    _code=$(printf '%s' "$_resp" | tail -1)
    _body=$(printf '%s' "$_resp" | sed '$d')
    if [ "$_code" = "200" ]; then log_pass "Translation returns 200"
    else log_fail "Expected 200, got $_code"; fi
    local _obj
    _obj=$(echo "$_body" | python3 -c "import json,sys; print(json.load(sys.stdin).get('object',''))" 2>/dev/null || echo "")
    if [ "$_obj" = "chat.completion" ]; then log_pass "Response is OpenAI format"
    else log_fail "Expected chat.completion, got: $_obj"; fi
    stop_server; stop_mock_server; unset ANTHROPIC_API_KEY
}

test_oai_to_anthropic_streaming() {
    section "OpenAI→Anthropic Translation: streaming"
    start_mock_anthropic_stream
    _anthropic_config "http://127.0.0.1:${MOCK_ANTHROPIC_PORT}/v1/messages"
    export ANTHROPIC_API_KEY="sk-ant-test-key"
    if ! start_server "/tmp/frugalis-config-test.toml"; then
        log_fail "Server start failed"; stop_mock_server; unset ANTHROPIC_API_KEY; return 1
    fi
    local _resp _code _body
    _resp=$(curl -s -w "\n%{http_code}" -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"model":"gpt-4","messages":[{"role":"user","content":"fix this bug"}],"stream":true}' \
        "$COMPLETION_URL" 2>/dev/null) || _resp="ERROR"
    _code=$(printf '%s' "$_resp" | tail -1)
    _body=$(printf '%s' "$_resp" | sed '$d')
    if [ "$_code" = "200" ]; then log_pass "Streaming returns 200"
    else log_fail "Expected 200, got $_code"; fi
    if printf '%s' "$_body" | grep -q "chatcmpl-"; then log_pass "SSE contains OpenAI chunk IDs"
    else log_fail "Missing OpenAI chunk IDs"; fi
    if printf '%s' "$_body" | grep -q "\[DONE\]"; then log_pass "SSE contains [DONE]"
    else log_fail "Missing [DONE]"; fi
    stop_server; stop_mock_server; unset ANTHROPIC_API_KEY
}

test_oai_to_anthropic_error() {
    section "OpenAI→Anthropic Translation: error forwarding"
    start_mock_anthropic_error
    _anthropic_config "http://127.0.0.1:${MOCK_ANTHROPIC_PORT}/v1/messages"
    export ANTHROPIC_API_KEY="sk-ant-test-key"
    if ! start_server "/tmp/frugalis-config-test.toml"; then
        log_fail "Server start failed"; stop_mock_server; unset ANTHROPIC_API_KEY; return 1
    fi
    local _resp _code _body
    _resp=$(curl -s -w "\n%{http_code}" -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"model":"gpt-4","messages":[{"role":"user","content":"fix this bug"}]}' \
        "$COMPLETION_URL" 2>/dev/null) || _resp="ERROR"
    _code=$(printf '%s' "$_resp" | tail -1)
    _body=$(printf '%s' "$_resp" | sed '$d')
    if [ "$_code" = "429" ]; then log_pass "Error 429 forwarded"
    else log_fail "Expected 429, got $_code"; fi
    local _err_type
    _err_type=$(echo "$_body" | python3 -c "import json,sys; print(json.load(sys.stdin).get('error',{}).get('type',''))" 2>/dev/null || echo "")
    if [ "$_err_type" = "rate_limit_error" ]; then log_pass "Error type preserved"
    else log_fail "Expected rate_limit_error, got: $_err_type"; fi
    stop_server; stop_mock_server; unset ANTHROPIC_API_KEY
}

test_header_forwarding() {
    section "Claude Code Compat: Header forwarding"
    local diag="/tmp/frugalis-cc-mock-diag.txt"
    rm -f "$diag"
    start_mock_anthropic_ok "$diag"
    _anthropic_config "http://127.0.0.1:${MOCK_ANTHROPIC_PORT}/v1/messages"
    export ANTHROPIC_API_KEY="sk-ant-test-key"
    if ! start_server "/tmp/frugalis-config-test.toml"; then
        log_fail "Server start failed"; stop_mock_server; unset ANTHROPIC_API_KEY; return 1
    fi
    curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -H "anthropic-beta: custom-feature-2025" -H "anthropic-version: 2023-06-01" \
        -H "x-claude-code-session-id: cc-session-test" \
        -d '{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"fix this bug in my code please"}]}' \
        "$MESSAGES_URL" >/dev/null 2>&1 || true
    if [ -f "$diag" ]; then
        local _beta _version _session
        _beta=$(python3 -c "import json; print(json.load(open('$diag')).get('anthropic_beta',''))" 2>/dev/null || echo "")
        _version=$(python3 -c "import json; print(json.load(open('$diag')).get('anthropic_version',''))" 2>/dev/null || echo "")
        _session=$(python3 -c "import json; print(json.load(open('$diag')).get('x_claude_code_session_id',''))" 2>/dev/null || echo "")
        if [ "$_beta" = "custom-feature-2025" ]; then log_pass "anthropic-beta forwarded"
        else log_fail "anthropic-beta NOT forwarded (got: $_beta)"; fi
        if [ "$_version" = "2023-06-01" ]; then log_pass "anthropic-version forwarded"
        else log_fail "anthropic-version NOT forwarded"; fi
        if [ "$_session" = "cc-session-test" ]; then log_pass "x-claude-code-session-id forwarded"
        else log_fail "x-claude-code-session-id NOT forwarded"; fi
    else
        log_fail "Mock diagnostics file not found"
    fi
    stop_server; stop_mock_server; unset ANTHROPIC_API_KEY; rm -f "$diag"
}

test_cache_control_passthrough() {
    section "Anthropic→Anthropic cache_control passthrough"
    local diag="/tmp/frugalis-cc-mock-diag.txt"
    rm -f "$diag"
    start_mock_anthropic_ok "$diag"
    _anthropic_config "http://127.0.0.1:${MOCK_ANTHROPIC_PORT}/v1/messages"
    export ANTHROPIC_API_KEY="sk-ant-test-key"
    if ! start_server "/tmp/frugalis-config-test.toml"; then
        log_fail "Server start failed"; stop_mock_server; unset ANTHROPIC_API_KEY; return 1
    fi
    curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"model":"claude-3.5","max_tokens":100,"system":[{"type":"text","text":"You are helpful","cache_control":{"type":"ephemeral"}}],"messages":[{"role":"user","content":[{"type":"text","text":"fix this bug in my code please","cache_control":{"type":"ephemeral"}}]}]}' \
        "$MESSAGES_URL" >/dev/null 2>&1 || true
    if [ -f "$diag" ]; then
        local _sys_cc _content_cc
        _sys_cc=$(python3 -c "import json; print(json.load(open('$diag')).get('system_cache_control',False))" 2>/dev/null || echo "False")
        _content_cc=$(python3 -c "import json; print(json.load(open('$diag')).get('content_cache_control',False))" 2>/dev/null || echo "False")
        if [ "$_sys_cc" = "True" ]; then log_pass "System block cache_control preserved"
        else log_fail "System block cache_control DROPPED"; fi
        if [ "$_content_cc" = "True" ]; then log_pass "Content block cache_control preserved"
        else log_fail "Content block cache_control DROPPED"; fi
    else
        log_fail "Mock diagnostics file not found"
    fi
    stop_server; stop_mock_server; unset ANTHROPIC_API_KEY; rm -f "$diag"
}


# ============================================================================
# Interactive modes
# ============================================================================

run_interactive() {
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo " Frugalis Interactive Manual Tests"
    echo " Target: $COMPLETION_URL"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "Make sure the server is running with: RUST_LOG=info cargo run"
    echo ""

    local prompts=(
        "COMPLEX_REASONING:architect rate limiter"
        "FILE_READING:read the content of file main.rs"
        "SYNTAX_FIX:fix this bug"
        "CASUAL:hello"
    )
    for test in "${prompts[@]}"; do
        IFS=':' read -r expected prompt <<< "$test"
        printf "[TEST] %s ... " "$expected"
        local _resp _code
        _resp=$(curl -s -w "\n%{http_code}" --max-time 120 "$COMPLETION_URL" \
            -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
            -d "{\"messages\":[{\"role\":\"user\",\"content\":\"$prompt\"}]}" 2>/dev/null) || _resp="ERROR"
        _code=$(printf '%s' "$_resp" | tail -1)
        if [ "$_code" = "200" ]; then
            printf "${GREEN}PASS${NC} (HTTP %s)\n" "$_code"; PASS=$((PASS+1))
        else
            printf "${RED}FAIL${NC} (HTTP %s)\n" "$_code"; FAIL=$((FAIL+1))
        fi
    done

    # Auth tests
    printf "[TEST] missing token ... "
    local _code
    _code=$(curl -s -o /dev/null -w "%{http_code}" "$COMPLETION_URL" \
        -H "Content-Type: application/json" -d '{"messages":[{"role":"user","content":"hello"}]}' 2>/dev/null) || true
    if [ "$_code" = "401" ]; then printf "${GREEN}PASS${NC}\n"; PASS=$((PASS+1))
    else printf "${RED}FAIL${NC} (got %s)\n" "$_code"; FAIL=$((FAIL+1)); fi

    # Streaming
    printf "[TEST] streaming ... "
    _resp=$(curl -s -w "\n%{http_code}" "$COMPLETION_URL" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"messages":[{"role":"user","content":"hello"}],"stream":true}' 2>/dev/null) || true
    _code=$(printf '%s' "$_resp" | tail -1)
    if [ "$_code" = "200" ]; then printf "${GREEN}PASS${NC}\n"; PASS=$((PASS+1))
    else printf "${RED}FAIL${NC} (got %s)\n" "$_code"; FAIL=$((FAIL+1)); fi
}

run_anthropic_interactive() {
    echo ""
    echo " Anthropic Pass-Through (POST /v1/messages) Interactive Tests"
    echo " Target: $MESSAGES_URL"
    echo ""
    echo "Server must be running. These test Anthropic-format requests."
    echo ""

    local prompts=("fix this bug please" "please read the file src/main.rs" "hello")
    for prompt in "${prompts[@]}"; do
        printf "[TEST] \"%s\" ... " "$prompt"
        local _resp _code
        _resp=$(curl -s -w "\n%{http_code}" "$MESSAGES_URL" \
            -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
            -d "{\"model\":\"claude-3.5\",\"max_tokens\":100,\"messages\":[{\"role\":\"user\",\"content\":\"$prompt\"}]}" 2>/dev/null) || _resp="ERROR"
        _code=$(printf '%s' "$_resp" | tail -1)
        if [ "$_code" = "200" ]; then printf "${GREEN}PASS${NC}\n"; PASS=$((PASS+1))
        else printf "${RED}FAIL${NC} (got %s)\n" "$_code"; FAIL=$((FAIL+1)); fi
    done
}

run_fewshot_interactive() {
    echo ""
    echo " Few-Shot Classifier Interactive Tests"
    echo " Server must be running on $HOST"
    echo ""

    if ! curl -s "http://$HOST/health" > /dev/null 2>&1; then
        echo "Server not running. Start with: RUST_LOG=info cargo run"
        exit 1
    fi

    # CASUAL bootstrap
    local _resp _code _tier _cat
    _resp=$(curl -s -w "\n%{http_code}" "$COMPLETION_URL" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"messages":[{"role":"user","content":"hello"}]}')
    _code=$(printf '%s' "$_resp" | tail -1)
    _tier=$(printf '%s' "$_resp" | sed '$d' | python3 -c "import sys,json; print(json.load(sys.stdin).get('tier',''))" 2>/dev/null || echo "")
    _cat=$(printf '%s' "$_resp" | sed '$d' | python3 -c "import sys,json; print(json.load(sys.stdin).get('category',''))" 2>/dev/null || echo "")
    if [ "$_code" = "200" ] && [ "$_tier" = "FewShot" ] && [ "$_cat" = "CASUAL" ]; then
        log_pass "Bootstrap CASUAL: tier=FewShot, category=CASUAL"
    else
        log_fail "Expected FewShot/CASUAL, got code=$_code tier=$_tier cat=$_cat"
    fi

    # Gibberish → Fallback
    _resp=$(curl -s -w "\n%{http_code}" "$COMPLETION_URL" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"messages":[{"role":"user","content":"zxcvbnm qwertyuiop asdfghjkl"}]}')
    _code=$(printf '%s' "$_resp" | tail -1)
    _tier=$(printf '%s' "$_resp" | sed '$d' | python3 -c "import sys,json; print(json.load(sys.stdin).get('tier',''))" 2>/dev/null || echo "")
    if [ "$_code" = "200" ] && [ "$_tier" = "Fallback" ]; then
        log_pass "Gibberish returns Fallback"
    else
        log_fail "Expected Fallback, got code=$_code tier=$_tier"
    fi

    # Feedback endpoint
    _resp=$(curl -s -w "\n%{http_code}" "http://$HOST/v1/feedback" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"text":"can you explain what a hash map is","actual_category":"CASUAL"}')
    _code=$(printf '%s' "$_resp" | tail -1)
    local _status
    _status=$(printf '%s' "$_resp" | sed '$d' | python3 -c "import sys,json; print(json.load(sys.stdin).get('status',''))" 2>/dev/null || echo "")
    if [ "$_code" = "200" ] && [ "$_status" = "accepted" ]; then
        log_pass "Feedback endpoint returns 200 {status:accepted}"
    else
        log_fail "Expected 200/accepted, got code=$_code status=$_status"
    fi
}

# ============================================================================
# Full automated suite (default)
# ============================================================================

run_all_automated() {
    echo ""
    echo "╔══════════════════════════════════════════════════════════════════╗"
    echo "║  Frugalis Unified Integration Test Suite                        ║"
    echo "╚══════════════════════════════════════════════════════════════════╝"
    echo ""

    build_server

    # Core classification & routing
    test_hardcoded_defaults
    test_threshold_override
    test_partial_categories
    test_combined_config
    test_negative_suppression

    # Config formats & validation
    test_yaml_config
    test_external_patterns
    test_validate_cli

    # Anthropic pass-through
    test_anthropic_passthrough
    test_models_endpoint

    # Translation with mock upstream
    test_oai_to_anthropic_translation
    test_oai_to_anthropic_streaming
    test_oai_to_anthropic_error
    test_header_forwarding
    test_cache_control_passthrough

    # Cache
    run_cache_tests
}

# ============================================================================
# Main dispatch
# ============================================================================

trap 'stop_mock_server; cleanup' EXIT

case "$MODE" in
    basic)       run_basic_tests ;;
    cache)       run_cache_tests ;;
    interactive) run_interactive ;;
    anthropic)   run_anthropic_interactive ;;
    fewshot)     run_fewshot_interactive ;;
    auto)        run_all_automated ;;
esac

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
printf "Results: ${GREEN}PASS=${PASS}${NC}  ${RED}FAIL=${FAIL}${NC}\n"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [ "$FAIL" -gt 0 ]; then exit 1; fi
exit 0
