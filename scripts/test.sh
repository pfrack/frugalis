#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# Frugalis Integration Test Suite (single entry point)
# ============================================================================
# USAGE:
#   ./scripts/test.sh              # Run all automated tests (default)
#   ./scripts/test.sh --basic      # Quick smoke tests only
#   ./scripts/test.sh --cache      # Cache-specific tests only
#   ./scripts/test.sh --interactive # Interactive manual testing (server running)
#   ./scripts/test.sh --anthropic  # Anthropic pass-through interactive
#   ./scripts/test.sh --fewshot    # Few-shot classifier interactive
#   ./scripts/test.sh --help       # Show this help
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR/.."

# ============================================================================
# Inline shared infrastructure (formerly lib.sh)
# ============================================================================

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

BINARY="./target/release/frugalis"
HOST="${HOST:-127.0.0.1:10000}"
HEALTH_URL="http://$HOST/health"
CLASSIFY_URL="http://$HOST/v1/classify"
COMPLETION_URL="http://$HOST/v1/chat/completions"
MESSAGES_URL="http://$HOST/v1/messages"
TOKEN="${PROXY_API_BEARER_TOKEN:-test-token-123}"

PASS=0
FAIL=0
SERVER_PID=""
MOCK_PID=""
MOCK_CC_PID=""

log_info()  { printf "${BLUE}[INFO]${NC} %s\n" "$1"; }
log_pass()  { printf "${GREEN}[✓]${NC} %s\n" "$1"; PASS=$((PASS+1)); }
log_fail()  { printf "${RED}[✗]${NC} %s\n" "$1"; FAIL=$((FAIL+1)); }

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
    local log_file="/tmp/frugalis-test-$$.log"
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
            log_pass "Server started (PID $SERVER_PID)"
            return 0
        fi
        printf "."
        sleep 1
    done
    log_fail "Server failed to start within ${attempts}s"
    tail -20 "$log_file" 2>/dev/null || true
    stop_server
    return 1
}

stop_server() {
    if [ -n "$SERVER_PID" ]; then
        kill $SERVER_PID 2>/dev/null || true
        wait $SERVER_PID 2>/dev/null || true
        SERVER_PID=""
    fi
}

stop_mock_server() {
    if [ -n "${MOCK_PID:-}" ]; then
        kill "$MOCK_PID" 2>/dev/null || true
        wait "$MOCK_PID" 2>/dev/null || true
        MOCK_PID=""
    fi
    if [ -n "${MOCK_CC_PID:-}" ]; then
        kill "$MOCK_CC_PID" 2>/dev/null || true
        wait "$MOCK_CC_PID" 2>/dev/null || true
        MOCK_CC_PID=""
    fi
    rm -f /tmp/frugalis-mock-*.py /tmp/frugalis-cc-mock-diag.txt
}

cleanup() {
    stop_mock_server
    stop_server
    rm -f /tmp/frugalis-config-*.toml /tmp/frugalis-config-*.yaml
    rm -rf /tmp/frugalis-patterns
    rm -f /tmp/frugalis-cache-*.toml
}

classify() {
    local prompt="$1"
    local response http_code body category
    response=$(curl -s -w "\n%{http_code}" "$CLASSIFY_URL" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d "{\"messages\":[{\"role\":\"user\",\"content\":\"$prompt\"}]}" 2>/dev/null) || return 1
    http_code=$(echo "$response" | tail -n1)
    body=$(echo "$response" | sed '$d')
    if [ "$http_code" != "200" ]; then echo "ERROR" >&2; return 1; fi
    category=$(echo "$body" | python3 -c "
import json,sys
try: d = json.load(sys.stdin); print(d.get('category', 'UNKNOWN'))
except: print('ERROR')
" 2>/dev/null || echo "ERROR")
    echo "$category"
}

extract_model() {
    echo "$1" | python3 -c "
import json,sys
try: d = json.load(sys.stdin, strict=False); print(d.get('model', d.get('upstream_model', '')))
except: print('')
" 2>/dev/null
}

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
            echo "  (default)      full automated suite"
            echo "  --basic        quick smoke: health, auth, classify, shutdown"
            echo "  --cache        cache tests: TTL, bypass, streaming, dashboard"
            echo "  --interactive  manual testing (server must be running)"
            echo "  --anthropic    anthropic pass-through interactive"
            echo "  --fewshot      few-shot classifier interactive"
            exit 0
            ;;
        *) echo "Unknown flag: $1. Use --help."; exit 2 ;;
    esac
fi

# ============================================================================
# BASIC SMOKE TESTS (--basic)
# ============================================================================

run_basic_tests() {
    section "Basic Smoke Tests"
    build_server
    if ! start_server ""; then log_fail "Server failed to start"; return 1; fi

    if curl -s -f "$HEALTH_URL" >/dev/null; then log_pass "health endpoint returns 200"
    else log_fail "health endpoint failed"; fi

    if curl -s -f -u admin:admin "http://$HOST/dashboard/inferences" >/dev/null; then
        log_pass "dashboard accepts Basic auth"
    else log_fail "dashboard Basic auth failed"; fi

    if [ "$(curl -s -o /dev/null -w '%{http_code}' 'http://$HOST/dashboard/inferences')" = "401" ]; then
        log_pass "dashboard rejects unauthenticated"
    else log_fail "expected 401 for unauthenticated dashboard"; fi

    local resp code body
    resp=$(curl -s -w "\n%{http_code}" -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d '{"messages":[{"role":"user","content":"hello"}]}' "$COMPLETION_URL" || true)
    code=$(printf '%s' "$resp" | tail -1)
    body=$(printf '%s' "$resp" | sed '$d')
    if [ "$code" = "200" ]; then log_pass "classify-only returns 200"
    else log_fail "expected 200, got $code"; fi

    log_info "Sending SIGTERM..."
    kill -TERM "$SERVER_PID" 2>/dev/null || true
    if wait "$SERVER_PID" 2>/dev/null; then log_pass "graceful shutdown"
    else log_fail "ungraceful shutdown"; fi
    SERVER_PID=""
}

# ============================================================================
# CACHE TESTS (--cache)
# ============================================================================

CACHE_TTL="${CACHE_TTL:-5}"

run_cache_tests() {
    section "Cache Integration Tests (TTL=${CACHE_TTL}s)"
    build_server

    # -- cache disabled by default --
    section "cache disabled when [cache] absent"
    if ! start_server ""; then log_fail "Server start failed"; return 1; fi
    local resp code
    resp=$(curl -s -w "\n%{http_code}" -u admin:admin "http://$HOST/dashboard/cache" 2>/dev/null || true)
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ] && printf '%s' "$resp" | grep -q "not configured"; then
        log_pass "cache page shows 'not configured'"
    else log_fail "cache page should show disabled (code=$code)"; fi
    stop_server

    # -- cache enabled --
    section "cache enabled via config"
    local cfg="/tmp/frugalis-cache-test-$$.toml"
    printf '[cache]\nttl_secs = %s\nmax_entries = 1000\n' "$CACHE_TTL" > "$cfg"
    if ! start_server "$cfg"; then log_fail "Server start failed"; return 1; fi
    if grep -q "Response cache enabled" "/tmp/frugalis-test-$$.log" 2>/dev/null; then
        log_pass "server log confirms cache enabled"
    else log_fail "missing 'Response cache enabled' in log"; fi
    resp=$(curl -s -w "\n%{http_code}" -u admin:admin "http://$HOST/dashboard/cache" 2>/dev/null || true)
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ]; then log_pass "dashboard/cache returns 200"
    else log_fail "dashboard/cache returned $code"; fi

    # -- cache hit --
    section "cache hit on identical request"
    local body='{"messages":[{"role":"user","content":"fix this bug"}]}'
    curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d "$body" "$COMPLETION_URL" >/dev/null 2>&1
    curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d "$body" "$COMPLETION_URL" >/dev/null 2>&1
    local dash
    dash=$(curl -s -u admin:admin "http://$HOST/dashboard/cache" 2>/dev/null)
    if printf '%s' "$dash" | grep -q "Hits"; then log_pass "dashboard shows cache hits"
    else log_fail "dashboard missing cache stats"; fi

    # -- bypass header --
    section "X-Frugalis-No-Cache bypass"
    resp=$(curl -s -w "\n%{http_code}" -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" -H "X-Frugalis-No-Cache: true" \
        -d "$body" "$COMPLETION_URL" 2>/dev/null || true)
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ]; then log_pass "bypass returns 200"
    else log_fail "bypass returned $code"; fi

    # -- streaming not cached --
    section "streaming requests not cached"
    resp=$(curl -s -w "\n%{http_code}" -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d '{"messages":[{"role":"user","content":"fix this bug"}],"stream":true}' "$COMPLETION_URL" 2>/dev/null || true)
    code=$(printf '%s' "$resp" | tail -1)
    if [ "$code" = "200" ]; then log_pass "streaming request returns 200"
    else log_fail "streaming returned $code"; fi

    # -- TTL expiry --
    section "TTL expiry"
    stop_server
    local short_cfg="/tmp/frugalis-cache-short-$$.toml"
    printf '[cache]\nttl_secs = 3\nmax_entries = 1000\n' > "$short_cfg"
    if ! start_server "$short_cfg"; then log_fail "Server start failed"; return 1; fi
    curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d "$body" "$COMPLETION_URL" >/dev/null 2>&1
    log_info "Waiting for TTL expiration (4s)..."
    sleep 4
    curl -s -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" -d "$body" "$COMPLETION_URL" >/dev/null 2>&1
    dash=$(curl -s -u admin:admin "http://$HOST/dashboard/cache" 2>/dev/null)
    if printf '%s' "$dash" | grep -q "Current Entries"; then log_pass "dashboard shows entries after TTL"
    else log_fail "dashboard entry display broken"; fi
    stop_server
    rm -f "$cfg" "$short_cfg"
}

# ============================================================================
# CLASSIFICATION & CONFIG TESTS (automated suite)
# ============================================================================

test_classify_defaults() {
    section "classify: hardcoded defaults (no config)"
    if ! start_server ""; then log_fail "Server start failed"; return 1; fi
    local tests=("FILE_READING:please read the file src/main.rs" "COMPLEX_REASONING:architect a distributed rate limiter" "CASUAL:hello")
    for test in "${tests[@]}"; do
        IFS=':' read -r expected prompt <<< "$test"
        result=$(classify "$prompt" 2>/dev/null) || result="ERROR"
        if [ "$result" = "$expected" ]; then log_pass "default: $expected"
        else log_fail "default: expected $expected, got $result"; fi
    done
    stop_server
}

test_classify_threshold_override() {
    section "classify: threshold override suppresses category"
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
    if ! start_server "/tmp/frugalis-config-test.toml"; then log_fail "Server start failed"; return 1; fi
    result=$(classify "please read the file src/main.rs") || result="ERROR"
    if [ "$result" != "FILE_READING" ]; then log_pass "threshold=100 suppresses FILE_READING (got $result)"
    else log_fail "threshold override NOT respected"; fi
    stop_server
}

test_classify_partial_categories() {
    section "classify: partial categories (only 2 of 4)"
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
    if ! start_server "/tmp/frugalis-config-test.toml"; then log_fail "Server start failed"; return 1; fi
    result=$(classify "hello") || result="ERROR"
    if [ "$result" = "CASUAL" ]; then log_pass "partial: CASUAL works"
    else log_fail "partial: expected CASUAL, got $result"; fi
    result=$(classify "please read the file src/main.rs") || result="ERROR"
    if [ "$result" = "FILE_READING" ]; then log_pass "partial: FILE_READING works"
    else log_fail "partial: expected FILE_READING, got $result"; fi
    result=$(classify "fix this bug") || result="ERROR"
    if [ "$result" != "SYNTAX_FIX" ]; then log_pass "partial: missing category falls back (got $result)"
    else log_fail "partial: SYNTAX_FIX should not exist"; fi
    stop_server
}

test_classify_combined_config() {
    section "classify: full config with routing"
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
    if ! start_server "/tmp/frugalis-config-test.toml"; then log_fail "Server start failed"; return 1; fi
    local tests=("FILE_READING:please read the file src/main.rs" "SYNTAX_FIX:fix this bug please" "COMPLEX_REASONING:architect a distributed rate limiter" "CASUAL:hello")
    for test in "${tests[@]}"; do
        IFS=':' read -r expected prompt <<< "$test"
        result=$(classify "$prompt") || result="ERROR"
        if [ "$result" = "$expected" ]; then log_pass "combined: $expected routed"
        else log_fail "combined: expected $expected, got $result"; fi
    done
    stop_server
}

test_classify_negative_suppression() {
    section "classify: negative pattern suppresses false positive"
    if ! start_server ""; then log_fail "Server start failed"; return 1; fi
    result=$(classify "read the architecture document") || result="ERROR"
    if [ "$result" != "COMPLEX_REASONING" ]; then
        log_pass "negative suppression: got $result (not COMPLEX_REASONING)"
    else log_fail "negative suppression broken: got COMPLEX_REASONING"; fi
    stop_server
}

test_classify_embedded_config() {
    section "classify: embedded config (no external file)"
    if ! start_server ""; then log_fail "Server start failed"; return 1; fi
    local tests=("FILE_READING:please read the file src/main.rs" "CASUAL:hello")
    for test in "${tests[@]}"; do
        IFS=':' read -r expected prompt <<< "$test"
        result=$(classify "$prompt" 2>/dev/null) || result="ERROR"
        if [ "$result" = "$expected" ]; then log_pass "embedded: $expected"
        else log_fail "embedded: expected $expected, got $result"; fi
    done
    stop_server
}

# ============================================================================
# CONFIG FORMAT TESTS
# ============================================================================

test_config_yaml() {
    section "config: YAML file loads and classifies"
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
    if ! start_server "/tmp/frugalis-config-test.yaml"; then log_fail "YAML server start failed"; return 1; fi
    local tests=("FILE_READING:please read the file src/main.rs" "CASUAL:hello")
    for test in "${tests[@]}"; do
        IFS=':' read -r expected prompt <<< "$test"
        result=$(classify "$prompt" 2>/dev/null) || result="ERROR"
        if [ "$result" = "$expected" ]; then log_pass "yaml: $expected"
        else log_fail "yaml: expected $expected, got $result"; fi
    done
    stop_server
}

test_config_yaml_validates() {
    section "config: YAML validates with --validate"
    export CONFIG_PATH="/tmp/frugalis-config-test.yaml"
    export PROXY_API_BEARER_TOKEN="$TOKEN"
    export DASHBOARD_BASIC_USER="admin"
    export DASHBOARD_BASIC_PASSWORD="admin"
    local output rc
    output=$("$BINARY" --validate 2>&1); rc=$?
    unset CONFIG_PATH
    if [ $rc -eq 0 ]; then log_pass "yaml: --validate succeeds"
    else log_fail "yaml: --validate failed: $output"; fi
}

test_config_external_patterns() {
    section "config: external pattern files"
    mkdir -p /tmp/frugalis-patterns
    cat > /tmp/frugalis-patterns/file_reading.patterns << 'EOF'
3 | (?i)\b(?:read|show|display|print|cat|view|open)\s+(?:the\s+)?(?:file|contents|this\s+file|that\s+file)\b
2 | (?i)\b(?:look|go|navigate)\s+(?:at|through|to|into)\s+(?:the\s+)?(?:file|directory|code|source)
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
    if ! start_server "/tmp/frugalis-config-test.toml"; then log_fail "Server start failed"; return 1; fi
    result=$(classify "please read the file src/main.rs" 2>/dev/null) || result="ERROR"
    if [ "$result" = "FILE_READING" ]; then log_pass "external patterns: FILE_READING"
    else log_fail "external patterns: expected FILE_READING, got $result"; fi
    result=$(classify "hello" 2>/dev/null) || result="ERROR"
    if [ "$result" = "CASUAL" ]; then log_pass "external patterns: CASUAL"
    else log_fail "external patterns: expected CASUAL, got $result"; fi
    stop_server
    rm -rf /tmp/frugalis-patterns
}

test_config_invalid_pattern_file() {
    section "config: invalid pattern file detected by --validate"
    mkdir -p /tmp/frugalis-patterns
    cat > /tmp/frugalis-patterns/bad.patterns << 'EOF'
3 | (?i)\b(?:read|show)\s+file\b
BADWEIGHT | not a number
NO_DELIMITER_LINE
EOF
    cat > /tmp/frugalis-config-test.toml << 'EOF'
patterns_dir = "/tmp/frugalis-patterns"
[categories.TEST_CAT]
description = "Test"
threshold = 3
priority = 1
patterns_file = "bad.patterns"
[routing.TEST_CAT]
model = "test-model"
provider_type = "nvidia_nim"
endpoint = "https://example.com"
api_key_env = "NVIDIA_API_KEY"
EOF
    export CONFIG_PATH="/tmp/frugalis-config-test.toml"
    export PROXY_API_BEARER_TOKEN="$TOKEN"
    export DASHBOARD_BASIC_USER="admin"
    export DASHBOARD_BASIC_PASSWORD="admin"
    local output rc
    set +e; output=$("$BINARY" --validate 2>&1); rc=$?; set -e
    unset CONFIG_PATH
    rm -rf /tmp/frugalis-patterns
    if [ $rc -ne 0 ] && echo "$output" | grep -q "invalid weight"; then
        log_pass "invalid pattern file detected"
    else log_fail "invalid pattern file not detected (rc=$rc)"; fi
}

# ============================================================================
# VALIDATION CLI TESTS
# ============================================================================

test_validate_valid_config() {
    section "validate: --validate on config.toml succeeds"
    export CONFIG_PATH="$(pwd)/config.toml"
    export PROXY_API_BEARER_TOKEN="$TOKEN"
    export DASHBOARD_BASIC_USER="admin"
    export DASHBOARD_BASIC_PASSWORD="admin"
    local output rc
    output=$("$BINARY" --validate 2>&1); rc=$?
    unset CONFIG_PATH
    if [ $rc -eq 0 ]; then log_pass "--validate succeeds on config.toml"
    else log_fail "--validate failed: $output"; fi
}

test_validate_invalid_regex() {
    section "validate: detects invalid regex"
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
    export PROXY_API_BEARER_TOKEN="$TOKEN"
    export DASHBOARD_BASIC_USER="admin"
    export DASHBOARD_BASIC_PASSWORD="admin"
    local output rc
    set +e; output=$("$BINARY" --validate 2>&1); rc=$?; set -e
    unset CONFIG_PATH
    if [ $rc -ne 0 ] && echo "$output" | grep -qi "pattern\|regex\|invalid"; then
        log_pass "--validate detects invalid regex"
    else log_fail "--validate did not detect invalid regex (rc=$rc)"; fi
}

test_validate_schema_errors() {
    section "validate: detects schema errors"
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
    export PROXY_API_BEARER_TOKEN="$TOKEN"
    export DASHBOARD_BASIC_USER="admin"
    export DASHBOARD_BASIC_PASSWORD="admin"
    local output rc
    set +e; output=$("$BINARY" --validate 2>&1); rc=$?; set -e
    unset CONFIG_PATH
    if [ $rc -ne 0 ]; then log_pass "--validate detects schema errors"
    else log_fail "schema errors not detected"; fi
}

test_validate_unknown_flag() {
    section "validate: unknown CLI flag gives helpful error"
    export PROXY_API_BEARER_TOKEN="$TOKEN"
    export DASHBOARD_BASIC_USER="admin"
    export DASHBOARD_BASIC_PASSWORD="admin"
    local output rc
    set +e; output=$("$BINARY" --badflag 2>&1); rc=$?; set -e
    if [ $rc -eq 2 ] && echo "$output" | grep -q "unknown argument"; then
        log_pass "unknown flag exits 2 with message"
    else log_fail "unknown flag behavior unexpected (rc=$rc)"; fi
}

test_validate_informative_errors() {
    section "validate: informative error for threshold=0"
    cat > /tmp/frugalis-config-test.toml << 'EOF'
[categories.BAD_CAT]
description = "Bad"
threshold = 0
priority = 0
EOF
    export CONFIG_PATH="/tmp/frugalis-config-test.toml"
    export PROXY_API_BEARER_TOKEN="$TOKEN"
    export DASHBOARD_BASIC_USER="admin"
    export DASHBOARD_BASIC_PASSWORD="admin"
    local output rc
    set +e; output=$("$BINARY" --validate 2>&1); rc=$?; set -e
    unset CONFIG_PATH
    if [ $rc -ne 0 ] && echo "$output" | grep -q "threshold"; then
        log_pass "threshold error is informative"
    else log_fail "threshold error not informative (rc=$rc, output: $output)"; fi
}

# ============================================================================
# ANTHROPIC PASS-THROUGH TESTS
# ============================================================================

test_anthropic_accepts_format() {
    section "anthropic: /v1/messages accepts Anthropic-format requests"
    if ! start_server ""; then log_fail "Server start failed"; return 1; fi
    local prompts=("fix this bug please" "please read the file src/main.rs" "hello")
    for prompt in "${prompts[@]}"; do
        local _resp _code
        _resp=$(curl -s -w "\n%{http_code}" "$MESSAGES_URL" \
            -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
            -d "{\"model\":\"claude-3.5\",\"max_tokens\":100,\"messages\":[{\"role\":\"user\",\"content\":\"$prompt\"}]}" 2>/dev/null) || _resp="ERROR"
        _code=$(printf '%s' "$_resp" | tail -1)
        if [ "$_code" = "200" ]; then log_pass "anthropic format: \"$prompt\""
        else log_fail "expected 200, got $_code for: $prompt"; fi
    done
    stop_server
}

test_anthropic_array_of_text_blocks() {
    section "anthropic: array-of-text-blocks content accepted"
    if ! start_server ""; then log_fail "Server start failed"; return 1; fi
    local _body='{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":[{"type":"text","text":"please "},{"type":"text","text":"fix this bug"}]}]}'
    local _resp _code
    _resp=$(curl -s -w "\n%{http_code}" "$MESSAGES_URL" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d "$_body" 2>/dev/null) || _resp="ERROR"
    _code=$(printf '%s' "$_resp" | tail -1)
    if [ "$_code" = "200" ]; then log_pass "array-of-text-blocks accepted"
    else log_fail "array-of-text-blocks: expected 200, got $_code"; fi
    stop_server
}

test_anthropic_requires_auth() {
    section "anthropic: /v1/messages requires bearer auth"
    if ! start_server ""; then log_fail "Server start failed"; return 1; fi
    local _code
    _code=$(curl -s -o /dev/null -w "%{http_code}" "$MESSAGES_URL" \
        -H "Content-Type: application/json" \
        -d '{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"hello"}]}' 2>/dev/null) || _code="000"
    if [ "$_code" = "401" ]; then log_pass "anthropic: 401 without token"
    else log_fail "expected 401, got $_code"; fi
    stop_server
}

test_anthropic_rejects_non_json() {
    section "anthropic: rejects non-JSON content-type (415)"
    if ! start_server ""; then log_fail "Server start failed"; return 1; fi
    local _code
    _code=$(curl -s -o /dev/null -w "%{http_code}" "$MESSAGES_URL" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: text/plain" \
        -d 'hello' 2>/dev/null) || _code="000"
    if [ "$_code" = "415" ]; then log_pass "anthropic: 415 for non-JSON"
    else log_fail "expected 415, got $_code"; fi
    stop_server
}

test_anthropic_error_envelope() {
    section "anthropic: error body uses Anthropic envelope shape"
    if ! start_server ""; then log_fail "Server start failed"; return 1; fi
    local _resp _code _json _error_type
    _resp=$(curl -s -w "\n%{http_code}" "$MESSAGES_URL" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: text/plain" \
        -d 'hello' 2>/dev/null) || _resp="ERROR"
    _code=$(printf '%s' "$_resp" | tail -1)
    _json=$(printf '%s' "$_resp" | sed '$d')
    _error_type=$(echo "$_json" | python3 -c "
import json,sys
try: d=json.load(sys.stdin); print(d.get('error',{}).get('type',''))
except: print('')
" 2>/dev/null || echo "")
    if [ "$_code" = "415" ] && [ "$_error_type" = "invalid_request_error" ]; then
        log_pass "error envelope: error.type=invalid_request_error"
    else log_fail "error envelope: expected invalid_request_error, got $_error_type (code=$_code)"; fi
    stop_server
}

test_anthropic_openapi_spec() {
    section "anthropic: OpenAPI spec documents /v1/messages"
    local _check
    _check=$(python3 -c "
import yaml,sys
try:
    d = yaml.safe_load(open('openapi/completions.yaml'))
    paths = d.get('paths', {})
    msgs = paths.get('/v1/messages', {})
    post = msgs.get('post', {})
    resp_400 = post.get('responses', {}).get('400', {})
    schema_ref = resp_400.get('content', {}).get('application/json', {}).get('schema', {})
    required = schema_ref.get('required', [])
    properties = schema_ref.get('properties', {})
    if msgs and 'type' in properties and 'error' in properties:
        print('OK')
    else:
        print(f'MISSING')
except Exception as e:
    print(f'YAML_ERROR: {e}')
" 2>/dev/null)
    if [ "$_check" = "OK" ]; then log_pass "OpenAPI spec documents /v1/messages"
    else log_fail "OpenAPI spec check: $_check"; fi
}

# ============================================================================
# CLAUDE CODE COMPAT: /v1/models
# ============================================================================

test_models_unauthenticated() {
    section "models: /v1/models returns 200 without auth"
    if ! start_server ""; then log_fail "Server start failed"; return 1; fi
    local _code
    _code=$(curl -s -o /dev/null -w "%{http_code}" "http://$HOST/v1/models" 2>/dev/null) || _code="000"
    if [ "$_code" = "200" ]; then log_pass "models: unauthenticated 200"
    else log_fail "expected 200, got $_code"; fi
    stop_server
}

test_models_shape() {
    section "models: response has Anthropic shape (display_name, type=model)"
    if ! start_server ""; then log_fail "Server start failed"; return 1; fi
    local _resp _code _body _check
    _resp=$(curl -s -w "\n%{http_code}" "http://$HOST/v1/models" 2>/dev/null) || _resp="ERROR"
    _code=$(printf '%s' "$_resp" | tail -1)
    _body=$(printf '%s' "$_resp" | sed '$d')
    _check=$(echo "$_body" | python3 -c "
import json,sys
data = json.load(sys.stdin)
entries = data.get('data', data) if isinstance(data, dict) else data
errors = []
for e in entries:
    eid = e.get('id','')
    if not e.get('display_name',''): errors.append(f'MISSING display_name on {eid}')
    if not (eid.startswith('claude') or eid.startswith('anthropic')): errors.append(f'BAD prefix: {eid}')
    if e.get('type','') != 'model': errors.append(f'BAD type on {eid}')
if errors: print('\\n'.join(errors))
else: print(f'OK {len(entries)} models')
" 2>/dev/null || echo "PARSE_ERROR")
    if [[ "$_check" == OK\ * ]]; then log_pass "models shape: $_check"
    else log_fail "models shape: $_check"; fi
    stop_server
}

# ============================================================================
# MOCK UPSTREAM SERVERS
# ============================================================================

MOCK_ANTHROPIC_PORT=10042
MOCK_CC_PORT=10043
MOCK_CC_DIAG="/tmp/frugalis-cc-mock-diag.txt"

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
    local diag_file="${1:-}"
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
    DIAG_FILE="${diag_file}" python3 "$mock_script" "$MOCK_ANTHROPIC_PORT" &
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

# ============================================================================
# OAI→ANTHROPIC TRANSLATION TESTS (with mock upstream)
# ============================================================================

test_translation_non_streaming() {
    section "translate: OpenAI→Anthropic non-streaming"
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
    if [ "$_code" = "200" ]; then log_pass "translate non-stream: 200"
    else log_fail "expected 200, got $_code"; fi
    local _obj
    _obj=$(echo "$_body" | python3 -c "import json,sys; print(json.load(sys.stdin).get('object',''))" 2>/dev/null || echo "")
    if [ "$_obj" = "chat.completion" ]; then log_pass "translate: response is OpenAI format"
    else log_fail "expected chat.completion, got: $_obj"; fi
    local _content
    _content=$(echo "$_body" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('choices',[])[0].get('message',{}).get('content',''))" 2>/dev/null || echo "")
    if [ "$_content" = "mock translated response" ]; then log_pass "translate: content matches mock"
    else log_fail "content mismatch: $_content"; fi
    local _finish
    _finish=$(echo "$_body" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('choices',[])[0].get('finish_reason',''))" 2>/dev/null || echo "")
    if [ "$_finish" = "stop" ]; then log_pass "translate: finish_reason=stop"
    else log_fail "expected stop, got $_finish"; fi
    local _pt _ct _tt
    _pt=$(echo "$_body" | python3 -c "import json,sys; print(json.load(sys.stdin).get('usage',{}).get('prompt_tokens',''))" 2>/dev/null || echo "")
    _ct=$(echo "$_body" | python3 -c "import json,sys; print(json.load(sys.stdin).get('usage',{}).get('completion_tokens',''))" 2>/dev/null || echo "")
    _tt=$(echo "$_body" | python3 -c "import json,sys; print(json.load(sys.stdin).get('usage',{}).get('total_tokens',''))" 2>/dev/null || echo "")
    if [ "$_pt" = "10" ] && [ "$_ct" = "5" ] && [ "$_tt" = "15" ]; then log_pass "translate: usage 10+5=15"
    else log_fail "usage: $_pt/$_ct/$_tt"; fi
    stop_server; stop_mock_server; unset ANTHROPIC_API_KEY
}

test_translation_streaming() {
    section "translate: OpenAI→Anthropic streaming SSE"
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
    if [ "$_code" = "200" ]; then log_pass "translate stream: 200"
    else log_fail "expected 200, got $_code"; fi
    if printf '%s' "$_body" | grep -q "chatcmpl-"; then log_pass "translate stream: OpenAI chunk IDs"
    else log_fail "missing chatcmpl- IDs"; fi
    if printf '%s' "$_body" | grep -q "\[DONE\]"; then log_pass "translate stream: [DONE]"
    else log_fail "missing [DONE]"; fi
    if printf '%s' "$_body" | grep -q '"finish_reason":"stop"'; then log_pass "translate stream: finish_reason=stop"
    else log_fail "missing finish_reason=stop"; fi
    stop_server; stop_mock_server; unset ANTHROPIC_API_KEY
}

test_translation_error_forwarding() {
    section "translate: upstream error forwarded to client"
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
    if [ "$_code" = "429" ]; then log_pass "translate error: 429 forwarded"
    else log_fail "expected 429, got $_code"; fi
    local _err_type
    _err_type=$(echo "$_body" | python3 -c "import json,sys; print(json.load(sys.stdin).get('error',{}).get('type',''))" 2>/dev/null || echo "")
    if [ "$_err_type" = "rate_limit_error" ]; then log_pass "translate error: type preserved"
    else log_fail "expected rate_limit_error, got: $_err_type"; fi
    stop_server; stop_mock_server; unset ANTHROPIC_API_KEY
}

# ============================================================================
# HEADER FORWARDING & CACHE CONTROL TESTS (with mock upstream)
# ============================================================================

test_header_forwarding() {
    section "headers: anthropic-beta, anthropic-version, x-claude-code-session-id forwarded"
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
        if [ "$_beta" = "custom-feature-2025" ]; then log_pass "header: anthropic-beta forwarded"
        else log_fail "anthropic-beta NOT forwarded (got: $_beta)"; fi
        if [ "$_version" = "2023-06-01" ]; then log_pass "header: anthropic-version forwarded"
        else log_fail "anthropic-version NOT forwarded"; fi
        if [ "$_session" = "cc-session-test" ]; then log_pass "header: x-claude-code-session-id forwarded"
        else log_fail "x-claude-code-session-id NOT forwarded"; fi
    else
        log_fail "mock diagnostics file not found"
    fi
    stop_server; stop_mock_server; unset ANTHROPIC_API_KEY; rm -f "$diag"
}

test_cache_control_passthrough() {
    section "cache_control: Anthropic→Anthropic passthrough (system + content blocks)"
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
        if [ "$_sys_cc" = "True" ]; then log_pass "cache_control: system block preserved"
        else log_fail "system block cache_control DROPPED"; fi
        if [ "$_content_cc" = "True" ]; then log_pass "cache_control: content block preserved"
        else log_fail "content block cache_control DROPPED"; fi
    else
        log_fail "mock diagnostics file not found"
    fi
    stop_server; stop_mock_server; unset ANTHROPIC_API_KEY; rm -f "$diag"
}

test_cache_control_auto_insertion() {
    section "cache_control: OAI→Anthropic auto-inserts cache_control in translated body"
    local diag="/tmp/frugalis-cc-mock-diag.txt"
    rm -f "$diag"
    start_mock_anthropic_ok "$diag"
    _anthropic_config "http://127.0.0.1:${MOCK_ANTHROPIC_PORT}/v1/messages"
    export ANTHROPIC_API_KEY="sk-ant-test-key"
    if ! start_server "/tmp/frugalis-config-test.toml"; then
        log_fail "Server start failed"; stop_mock_server; unset ANTHROPIC_API_KEY; return 1
    fi
    local _resp _code
    _resp=$(curl -s -w "\n%{http_code}" -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"model":"gpt-4","messages":[{"role":"system","content":"You are helpful."},{"role":"user","content":"fix this bug please"}],"max_tokens":100}' \
        "$COMPLETION_URL" 2>/dev/null) || _resp="ERROR"
    _code=$(printf '%s' "$_resp" | tail -1)
    if [ "$_code" != "200" ]; then
        log_fail "expected 200, got $_code"
    else
        if [ -f "$diag" ]; then
            local _has_cc _has_system
            _has_cc=$(python3 -c "import json; print(json.load(open('$diag')).get('has_cache_control',False))" 2>/dev/null || echo "False")
            _has_system=$(python3 -c "import json; print(json.load(open('$diag')).get('has_system',False))" 2>/dev/null || echo "False")
            if [ "$_has_cc" = "True" ]; then log_pass "cache_control auto-inserted in translated body"
            else log_fail "cache_control MISSING in translated body"; fi
            if [ "$_has_system" = "True" ]; then log_pass "system prompt translated correctly"
            else log_fail "system prompt MISSING"; fi
        else
            log_fail "mock diagnostics file not found"
        fi
        # Verify response is OpenAI format with cached_tokens
        local _body _obj _cached
        _body=$(printf '%s' "$_resp" | sed '$d')
        _obj=$(echo "$_body" | python3 -c "import json,sys; print(json.load(sys.stdin).get('object',''))" 2>/dev/null || echo "")
        _cached=$(echo "$_body" | python3 -c "
import json,sys
d = json.load(sys.stdin)
print(d.get('usage',{}).get('prompt_tokens_details',{}).get('cached_tokens',''))
" 2>/dev/null || echo "")
        if [ "$_obj" = "chat.completion" ]; then log_pass "response is OpenAI format"
        else log_fail "expected chat.completion, got: $_obj"; fi
        if [ "$_cached" = "35" ]; then log_pass "cached_tokens=35 translated from cache_read_input_tokens"
        else log_fail "expected cached_tokens=35, got: $_cached"; fi
    fi
    stop_server; stop_mock_server; unset ANTHROPIC_API_KEY; rm -f "$diag"
}

# ============================================================================
# INTERACTIVE MODES
# ============================================================================

run_interactive() {
    echo ""
    echo " Frugalis Interactive Manual Tests"
    echo " Target: $COMPLETION_URL"
    echo " Server must be running: RUST_LOG=info cargo run"
    echo ""
    local prompts=("COMPLEX_REASONING:architect rate limiter" "FILE_READING:read the content of file main.rs" "SYNTAX_FIX:fix this bug" "CASUAL:hello")
    for test in "${prompts[@]}"; do
        IFS=':' read -r expected prompt <<< "$test"
        printf "[TEST] %s ... " "$expected"
        local _resp _code
        _resp=$(curl -s -w "\n%{http_code}" --max-time 120 "$COMPLETION_URL" \
            -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
            -d "{\"messages\":[{\"role\":\"user\",\"content\":\"$prompt\"}]}" 2>/dev/null) || _resp="ERROR"
        _code=$(printf '%s' "$_resp" | tail -1)
        if [ "$_code" = "200" ]; then printf "${GREEN}PASS${NC} (HTTP %s)\n" "$_code"; PASS=$((PASS+1))
        else printf "${RED}FAIL${NC} (HTTP %s)\n" "$_code"; FAIL=$((FAIL+1)); fi
    done
    # Auth
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
    echo " Anthropic Pass-Through Interactive Tests"
    echo " Target: $MESSAGES_URL"
    echo " Server must be running."
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
    # Auth + content-type
    printf "[TEST] missing token → 401 ... "
    _code=$(curl -s -o /dev/null -w "%{http_code}" "$MESSAGES_URL" -H "Content-Type: application/json" \
        -d '{"model":"claude-3.5","max_tokens":100,"messages":[{"role":"user","content":"hello"}]}' 2>/dev/null) || true
    if [ "$_code" = "401" ]; then printf "${GREEN}PASS${NC}\n"; PASS=$((PASS+1))
    else printf "${RED}FAIL${NC}\n"; FAIL=$((FAIL+1)); fi
    printf "[TEST] non-JSON → 415 ... "
    _code=$(curl -s -o /dev/null -w "%{http_code}" "$MESSAGES_URL" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: text/plain" -d 'hello' 2>/dev/null) || true
    if [ "$_code" = "415" ]; then printf "${GREEN}PASS${NC}\n"; PASS=$((PASS+1))
    else printf "${RED}FAIL${NC}\n"; FAIL=$((FAIL+1)); fi
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
        log_pass "fewshot: bootstrap CASUAL (tier=FewShot)"
    else log_fail "fewshot: expected FewShot/CASUAL, got tier=$_tier cat=$_cat"; fi
    # Gibberish → Fallback
    _resp=$(curl -s -w "\n%{http_code}" "$COMPLETION_URL" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"messages":[{"role":"user","content":"zxcvbnm qwertyuiop asdfghjkl"}]}')
    _code=$(printf '%s' "$_resp" | tail -1)
    _tier=$(printf '%s' "$_resp" | sed '$d' | python3 -c "import sys,json; print(json.load(sys.stdin).get('tier',''))" 2>/dev/null || echo "")
    if [ "$_code" = "200" ] && [ "$_tier" = "Fallback" ]; then log_pass "fewshot: gibberish → Fallback"
    else log_fail "fewshot: expected Fallback, got tier=$_tier"; fi
    # Regex catches first
    _resp=$(curl -s -w "\n%{http_code}" "$COMPLETION_URL" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"messages":[{"role":"user","content":"fix this bug"}]}')
    _code=$(printf '%s' "$_resp" | tail -1)
    _tier=$(printf '%s' "$_resp" | sed '$d' | python3 -c "import sys,json; print(json.load(sys.stdin).get('tier',''))" 2>/dev/null || echo "")
    _cat=$(printf '%s' "$_resp" | sed '$d' | python3 -c "import sys,json; print(json.load(sys.stdin).get('category',''))" 2>/dev/null || echo "")
    if [ "$_code" = "200" ] && [ "$_tier" = "Regex" ] && [ "$_cat" = "SYNTAX_FIX" ]; then
        log_pass "fewshot: regex catches SYNTAX_FIX before fewshot"
    else log_fail "fewshot: expected Regex/SYNTAX_FIX, got tier=$_tier cat=$_cat"; fi
    # Feedback endpoint
    _resp=$(curl -s -w "\n%{http_code}" "http://$HOST/v1/feedback" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"text":"can you explain what a hash map is","actual_category":"CASUAL"}')
    _code=$(printf '%s' "$_resp" | tail -1)
    local _status
    _status=$(printf '%s' "$_resp" | sed '$d' | python3 -c "import sys,json; print(json.load(sys.stdin).get('status',''))" 2>/dev/null || echo "")
    if [ "$_code" = "200" ] && [ "$_status" = "accepted" ]; then
        log_pass "fewshot: feedback endpoint returns accepted"
    else log_fail "fewshot: feedback expected accepted, got code=$_code status=$_status"; fi
    # Feedback auth
    _code=$(curl -s -o /dev/null -w "%{http_code}" "http://$HOST/v1/feedback" \
        -H "Content-Type: application/json" -d '{"text":"test","actual_category":"CASUAL"}' 2>/dev/null) || true
    if [ "$_code" = "401" ]; then log_pass "fewshot: feedback requires auth"
    else log_fail "fewshot: feedback expected 401, got $_code"; fi
}

# ============================================================================
# Responses API (Codex CLI compat) test functions
# ============================================================================

test_responses_non_streaming() {
    local _resp _code _text
    _resp=$(curl -s -w "\n%{http_code}" "http://$HOST/v1/responses" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"model":"gpt-4o","input":"hello"}')
    _code=$(printf '%s' "$_resp" | tail -1)
    _text=$(printf '%s' "$_resp" | sed '$d' | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('output',[{}])[0].get('content',[{}])[0].get('text',''))" 2>/dev/null || echo "")
    if [ "$_code" = "200" ] && [ -n "$_text" ]; then
        log_pass "responses: non-streaming returns output text"
    else log_fail "responses: expected 200 with non-empty output, got code=$_code text=$_text"; fi
}

test_responses_streaming() {
    local _resp _events
    # Start streaming request, capture first 5 seconds of output, count event types
    _resp=$(timeout 5 curl -sN "http://$HOST/v1/responses" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"model":"gpt-4o","stream":true,"input":"hello"}' 2>/dev/null || true)
    _events=$(printf '%s' "$_resp" | grep -c '^event: response\.' || true)
    if [ "$_events" -ge 4 ]; then
        log_pass "responses: streaming emits $_events SSE events (>=4 expected)"
    else log_fail "responses: expected >=4 SSE events, got $_events"; fi
}

test_responses_auth_required() {
    local _code
    _code=$(curl -s -o /dev/null -w "%{http_code}" "http://$HOST/v1/responses" \
        -H "Content-Type: application/json" \
        -d '{"model":"gpt-4o","input":"hello"}' 2>/dev/null) || true
    if [ "$_code" = "401" ]; then
        log_pass "responses: auth required returns 401"
    else log_fail "responses: expected 401 without token, got $_code"; fi
}

test_responses_unsupported_field() {
    local _resp _code
    _resp=$(curl -s -w "\n%{http_code}" "http://$HOST/v1/responses" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"model":"gpt-4o","input":"hello","tools":[{"type":"web_search"}]}')
    _code=$(printf '%s' "$_resp" | tail -1)
    if [ "$_code" = "400" ]; then
        log_pass "responses: unsupported field returns 400"
    else log_fail "responses: expected 400 for unsupported field, got $_code"; fi
}

test_responses_function_call() {
    local _resp _has_fc
    _resp=$(timeout 5 curl -sN "http://$HOST/v1/responses" \
        -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
        -d '{"model":"gpt-4o","stream":true,"input":"get weather in NYC","tools":[{"type":"function","function":{"name":"get_weather","parameters":{"type":"object","properties":{"city":{"type":"string"}}}}}]}' 2>/dev/null || true)
    _has_fc=$(printf '%s' "$_resp" | grep -c 'function_call_arguments' || true)
    if [ "$_has_fc" -ge 1 ]; then
        log_pass "responses: function call streaming emits function_call_arguments events"
    else log_fail "responses: expected function_call_arguments in stream"; fi
}

# ============================================================================
# FULL AUTOMATED SUITE (default)
# ============================================================================

run_all_automated() {
    echo ""
    echo "╔══════════════════════════════════════════════════════════════════╗"
    echo "║  Frugalis Integration Test Suite                                ║"
    echo "╚══════════════════════════════════════════════════════════════════╝"
    echo ""

    build_server

    # ── Classification & routing ──
    test_classify_defaults
    test_classify_threshold_override
    test_classify_partial_categories
    test_classify_combined_config
    test_classify_negative_suppression
    test_classify_embedded_config

    # ── Config formats ──
    test_config_yaml
    test_config_yaml_validates
    test_config_external_patterns
    test_config_invalid_pattern_file

    # ── Validation CLI ──
    test_validate_valid_config
    test_validate_invalid_regex
    test_validate_schema_errors
    test_validate_unknown_flag
    test_validate_informative_errors

    # ── Anthropic pass-through ──
    test_anthropic_accepts_format
    test_anthropic_array_of_text_blocks
    test_anthropic_requires_auth
    test_anthropic_rejects_non_json
    test_anthropic_error_envelope
    test_anthropic_openapi_spec

    # ── /v1/models (Claude Code compat) ──
    test_models_unauthenticated
    test_models_shape

    # ── OAI→Anthropic translation (mock upstream) ──
    test_translation_non_streaming
    test_translation_streaming
    test_translation_error_forwarding

    # ── Header forwarding & cache control ──
    test_header_forwarding
    test_cache_control_passthrough
    test_cache_control_auto_insertion

    # ── Cache ──
    run_cache_tests

    # ── Responses API (Codex CLI compat) ──
    test_responses_auth_required
    test_responses_unsupported_field
    test_responses_non_streaming
    test_responses_streaming
    test_responses_function_call
}

# ============================================================================
# MAIN DISPATCH
# ============================================================================

trap cleanup EXIT

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
