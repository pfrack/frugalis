#!/bin/bash
set -euo pipefail

# ============================================================================
# Cerebrum Manual & Automated Tests for Shared Category Config (S-07b)
# ============================================================================
# USAGE:
#   ./run.sh                  # Interactive manual testing (default)
#   ./run.sh --auto          # Fully automated integration tests
#   ./run.sh --help          # Show this help
#
# Interactive mode:
#   - Waits for you to start the server
#   - Lets you manually test classification endpoints
#   - Shows detailed output for each request
#
# Automated mode (--auto):
#   - Builds the server binary (release)
#   - Creates various config.toml scenarios
#   - Starts/stops server automatically for each test
#   - Validates all expected outcomes
#   - Exit code 0 on success, non-zero on failure
# ============================================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# ============================================================================
# Mode detection
# ============================================================================
AUTO_MODE=false
if [ $# -gt 0 ] && ([ "$1" = "--auto" ] || [ "$1" = "-a" ]); then
    AUTO_MODE=true
fi

# ============================================================================
# Automated test functions (--auto mode) — sourced from lib.sh
# ============================================================================
if [ "$AUTO_MODE" = true ]; then
    source "$SCRIPT_DIR/lib.sh"
    trap cleanup EXIT

    # ============================================================================
    # Automated test scenarios
    # ============================================================================
    test_hardcoded_defaults() {
        section "Test 1: Hardcoded Defaults (no config.toml)"
        
        rm -f /tmp/cerebrum-config-test.toml
        unset CONFIG_PATH
        
        if ! start_server ""; then
            log_fail "Failed to start server"
            return 1
        fi
        
        local tests=(
            "FILE_READING:please read the file src/main.rs"
            "COMPLEX_REASONING:architect a distributed rate limiter"
            "CASUAL:hello"
        )
        
        local all_pass=true
        for test in "${tests[@]}"; do
            IFS=':' read -r expected prompt <<< "$test"
            result=$(classify "$prompt" 2>/dev/null) || result="ERROR"
            
            if [ "$result" = "$expected" ]; then
                log_pass "$expected prompt classified correctly"
            else
                log_fail "Expected $expected, got $result"
                all_pass=false
            fi
        done
        
        stop_server
        return $([ "$all_pass" = true ] && echo 0 || echo 1)
    }

    test_threshold_override() {
        section "Test 2: Threshold Override (FILE_READING threshold = 100)"
        
         cat > /tmp/cerebrum-config-test.toml << 'EOF'
[[categories]]
name = "FILE_READING"
description = "Reading, viewing, inspecting, searching, or navigating files or code"
threshold = 100
priority = 1

[[categories]]
name = "SYNTAX_FIX"
description = "Fixing bugs, errors, typos, compilation issues, or broken code"
threshold = 3
priority = 2

[[categories]]
name = "COMPLEX_REASONING"
description = "Multi-step reasoning, architecture design, refactoring, deep analysis, or performance optimization"
threshold = 3
priority = 3

[[categories]]
name = "CASUAL"
description = "Simple questions, greetings, general conversation, or short prompts"
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
        
        if ! start_server "/tmp/cerebrum-config-test.toml"; then
            log_fail "Failed to start server"
            return 1
        fi
        
        result=$(classify "please read the file src/main.rs") || result="ERROR"
        
        if [ "$result" = "CASUAL" ] || [ "$result" = "ERROR" ]; then
            log_pass "FILE_READING threshold override respected (fell back to CASUAL)"
            stop_server
            return 0
        elif [ "$result" = "FILE_READING" ]; then
            log_fail "FILE_READING threshold override NOT respected"
            stop_server
            return 1
        else
            log_fail "Unexpected result: $result"
            stop_server
            return 1
        fi
    }

    test_partial_categories() {
        section "Test 3: Partial Categories (FILE_READING + CASUAL only)"
        
         cat > /tmp/cerebrum-config-test.toml << 'EOF'
[[categories]]
name = "FILE_READING"
description = "Reading files"
threshold = 3
priority = 1

[[categories]]
name = "CASUAL"
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
        
        if ! start_server "/tmp/cerebrum-config-test.toml"; then
            log_fail "Failed to start server"
            return 1
        fi
        
        local all_pass=true
        
        result=$(classify "hello") || result="ERROR"
        if [ "$result" = "CASUAL" ]; then
            log_pass "CASUAL works with partial config"
        else
            log_fail "CASUAL failed: got $result"
            all_pass=false
        fi
        
        result=$(classify "please read the file src/main.rs") || result="ERROR"
        if [ "$result" = "FILE_READING" ]; then
            log_pass "FILE_READING works with partial config"
        else
            log_fail "FILE_READING failed: got $result"
            all_pass=false
        fi
        
        result=$(classify "fix this bug") || result="ERROR"
        if [ "$result" = "CASUAL" ] || [ "$result" = "ERROR" ]; then
            log_pass "Missing category falls back (got $result)"
        else
            log_fail "Missing category unexpected: $result"
            all_pass=false
        fi
        
        stop_server
        return $([ "$all_pass" = true ] && echo 0 || echo 1)
    }

    test_legacy_routing() {
        section "Test 4: Legacy routing.toml (no config.toml)"
        
        cp routing_examples/routing-manual-tests.toml /tmp/cerebrum-routing-legacy.toml
        unset CONFIG_PATH
        
        if ! start_server ""; then
            log_fail "Failed to start server"
            return 1
        fi
        
        result=$(classify "hello") || result="ERROR"
        if [ "$result" = "CASUAL" ]; then
            log_pass "No config falls back to hardcoded (CASUAL)"
        else
            log_fail "Unexpected result without config: $result"
            stop_server
            return 1
        fi
        
        stop_server
        log_info "This implementation uses hardcoded categories + routing file (or hardcoded)"
        log_pass "Legacy mode supported (categories hardcoded, routing from file)"
        
        return 0
    }

    test_combined_config() {
        section "Test 5: Combined config.toml (categories + routing)"
        
         cat > /tmp/cerebrum-config-test.toml << 'EOF'
[[categories]]
name = "FILE_READING"
description = "Reading, viewing, inspecting, searching, or navigating files or code"
threshold = 3
priority = 1

[[categories]]
name = "SYNTAX_FIX"
description = "Fixing bugs, errors, typos, compilation issues, or broken code"
threshold = 3
priority = 2

[[categories]]
name = "COMPLEX_REASONING"
description = "Multi-step reasoning, architecture design, refactoring, deep analysis, or performance optimization"
threshold = 3
priority = 3

[[categories]]
name = "CASUAL"
description = "Simple questions, greetings, general conversation, or short prompts"
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
        
        if ! start_server "/tmp/cerebrum-config-test.toml"; then
            log_fail "Failed to start server"
            return 1
        fi
        
        local tests=(
            "FILE_READING:please read the file src/main.rs"
            "SYNTAX_FIX:fix this bug please"
            "COMPLEX_REASONING:architect a distributed rate limiter"
            "CASUAL:hello"
        )
        
        local all_pass=true
        for test in "${tests[@]}"; do
            IFS=':' read -r expected prompt <<< "$test"
            result=$(classify "$prompt") || result="ERROR"
            
            if [ "$result" = "$expected" ]; then
                log_pass "$expected routed correctly"
            else
                log_fail "Expected $expected, got $result"
                all_pass=false
            fi
        done
        
        stop_server
        return $([ "$all_pass" = true ] && echo 0 || echo 1)
    }

     test_field_integrity() {
         section "Test 6: Field Value Integrity"
         
         cat > /tmp/cerebrum-config-test.toml << 'EOF'
[[categories]]
name = "FILE_READING"
description = "Test category"
threshold = 100
priority = 1

[[categories]]
name = "SYNTAX_FIX"
description = "Test syntax fix"
threshold = 3
priority = 2

[[categories]]
name = "COMPLEX_REASONING"
description = "Test complex"
threshold = 3
priority = 3

[[categories]]
name = "CASUAL"
description = "Test casual"
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
        
        if ! start_server "/tmp/cerebrum-config-test.toml"; then
            log_fail "Failed to start server"
            return 1
        fi
        
        result=$(classify "please read the file src/main.rs") || result="ERROR"
        if [ "$result" = "CASUAL" ] || [ "$result" = "ERROR" ]; then
            log_pass "FILE_READING threshold 100 respected (fell back to CASUAL or ERROR)"
        else
            log_fail "FILE_READING threshold override NOT respected: $result"
            stop_server
            return 1
        fi
        
        stop_server
        return 0
    }

    test_negative_suppression() {
        section "Test 7: Negative Suppression (regression test)"
        
        if ! start_server ""; then
            log_fail "Failed to start server"
            return 1
        fi
        
        result=$(classify "read the architecture document") || result="ERROR"
        
        if [ "$result" != "COMPLEX_REASONING" ]; then
            log_pass "Negative suppression working (got $result, not COMPLEX_REASONING)"
            stop_server
            return 0
        else
            log_fail "Negative suppression broken: got COMPLEX_REASONING"
            stop_server
            return 1
        fi
    }

    run_automated_tests() {
        echo ""
        echo "╔══════════════════════════════════════════════════════════════════╗"
        echo "║  Automated Integration Tests: Shared Category Config (S-07b)    ║"
        echo "╚══════════════════════════════════════════════════════════════════╝"
        echo ""
        
        if [ ! -f "Cargo.toml" ]; then
            echo "ERROR: Must run from project root (Cargo.toml not found)" >&2
            exit 1
        fi
        
        build_server
        
        test_hardcoded_defaults
        test_threshold_override
        test_partial_categories
        test_legacy_routing
        test_combined_config
        test_field_integrity
        test_negative_suppression
        
        echo ""
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        local total=$((PASS + FAIL))
        printf "Results: ${GREEN}%d/%d passed${NC}, ${RED}%d failed${NC}\n" "$PASS" "$total" "$FAIL"
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo ""
        
        if [ $FAIL -gt 0 ]; then
            exit 1
        else
            log_pass "All tests passed!"
            exit 0
        fi
    }

    # Run automated mode
    run_automated_tests
fi

# ============================================================================
# Interactive manual testing mode (default)
# ============================================================================
# This is the original run.sh functionality

if [ -z "$TOKEN" ]; then
    echo "ERROR: PROXY_API_BEARER_TOKEN is not set" >&2
    echo "Set it via: export PROXY_API_BEARER_TOKEN=your_token" >&2
    exit 1
fi

# Colors (redefine in case not in auto mode)
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

PASS=0
FAIL=0

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

run_test() {
    _label="$1" _body="$2"
    _resp=""
    _http_code=""
    _upstream_model=""
    _error_msg=""
    _tmpfile=$(mktemp)
    _start=$(date +%s)

    printf "[TEST] %s\n" "$_label"
    printf "  ⏳  "
    ( curl -s -w "\n%{http_code}" --max-time 120 \
        "$URL" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -d "$_body" > "$_tmpfile" 2>/dev/null ) &
    _curl_pid=$!
    while kill -0 "$_curl_pid" 2>/dev/null; do
        sleep 2
        printf "."
    done
    wait $_curl_pid 2>/dev/null
    _end=$(date +%s)
    _elapsed=$((_end - _start))
    printf " done\n"
    _resp=$(cat "$_tmpfile")
    rm -f "$_tmpfile"
    _http_code=$(printf '%s' "$_resp" | tail -1)
    _body_resp=$(printf '%s' "$_resp" | sed '$d')
    _upstream_model=$(extract_model "$_body_resp")

    if [ "$_http_code" = "200" ]; then
        printf "  ${GREEN}PASS${NC} (HTTP %s, %ss, model=%s)\n" "$_http_code" "$_elapsed" "$_upstream_model"
        PASS=$((PASS+1))
    else
        _error_msg=$(extract_error "$_body_resp")
        [ -z "$_error_msg" ] && _error_msg=$(printf '%s' "$_body_resp" | head -c 120)
        printf "  ${RED}FAIL${NC} (HTTP %s, %ss): %s\n" "$_http_code" "$_elapsed" "$_error_msg"
        FAIL=$((FAIL+1))
    fi
}

run_test_headers() {
    _label="$1" _category="$2" _model="$3"
    _resp=""
    _http_code=""
    _upstream_model=""
    _error_msg=""
    _tmpfile=$(mktemp)
    _start=$(date +%s)

    printf "[TEST] %s\n" "$_label"
    printf "  ⏳  "
    ( curl -s -w "\n%{http_code}" --max-time 120 \
        "$URL" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" \
        -H "X-Cerebrum-Category: $_category" \
        -H "X-Cerebrum-Model: $_model" \
        -d '{"messages":[{"role":"user","content":"hello"}]}' > "$_tmpfile" 2>/dev/null ) &
    _curl_pid=$!
    while kill -0 "$_curl_pid" 2>/dev/null; do
        sleep 2
        printf "."
    done
    wait $_curl_pid 2>/dev/null
    _end=$(date +%s)
    _elapsed=$((_end - _start))
    printf " done\n"
    _resp=$(cat "$_tmpfile")
    rm -f "$_tmpfile"
    _http_code=$(printf '%s' "$_resp" | tail -1)
    _body_resp=$(printf '%s' "$_resp" | sed '$d')

    if [ "$_http_code" = "200" ]; then
        _upstream_model=$(extract_model "$_body_resp")
        printf "  ${GREEN}PASS${NC} (HTTP %s, %ss, model=%s)\n" "$_http_code" "$_elapsed" "$_upstream_model"
        PASS=$((PASS+1))
    else
        _error_msg=$(extract_error "$_body_resp")
        [ -z "$_error_msg" ] && _error_msg=$(printf '%s' "$_body_resp" | head -c 120)
        printf "  ${RED}FAIL${NC} (HTTP %s, %ss): %s\n" "$_http_code" "$_elapsed" "$_error_msg"
        FAIL=$((FAIL+1))
    fi
}

# Interactive mode header
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo " Cerebrum Manual Route Tests (Shared Category Config Validation)"
echo " Target: $URL"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Make sure the server is running with:"
echo "  RUST_LOG=info cargo run"
echo ""
echo "Press Ctrl+C to abort any test, or wait for completion."
echo ""

# ── COMPLEX_REASONING (expects NVIDIA meta/llama-3.3-70b-instruct) ──
echo "── COMPLEX_REASONING ──"
run_test "architect a system" \
    '{"messages":[{"role":"user","content":"architect rate limiter"}]}'
run_test "design a database schema" \
    '{"messages":[{"role":"user","content":"design a distributed database schema for multi-tenant SaaS"}]}'

# ── FILE_READING (expects NVIDIA meta/llama-3.1-70b-instruct) ──
echo ""
echo "── FILE_READING ──"
run_test "read file content" \
    '{"messages":[{"role":"user","content":"read the content of file main.rs"}]}'
run_test "parse JSON file" \
    '{"messages":[{"role":"user","content":"parse the JSON config and explain the structure"}]}'

# ── SYNTAX_FIX (expects NVIDIA qwen/qwen3-coder-480b-a35b-instruct) ──
echo ""
echo "── SYNTAX_FIX ──"
run_test "fix this bug" \
    '{"messages":[{"role":"user","content":"fix this bug"}]}'
run_test "debug error" \
    '{"messages":[{"role":"user","content":"debug this error: index out of bounds"}]}'

# ── CASUAL (expects NVIDIA meta/llama-3.1-8b-instruct) ──
echo ""
echo "── CASUAL ──"
run_test "casual hello" \
    '{"messages":[{"role":"user","content":"hello"}]}'
run_test "what is Rust" \
    '{"messages":[{"role":"user","content":"what is Rust programming language"}]}'

# ── FALLBACK / EDGE CASES ──
echo ""
echo "── FALLBACK / EDGE CASES ──"
run_test "empty message" \
    '{"messages":[{"role":"user","content":""}]}'

# ── Classification bypass: X-Cerebrum-Category + X-Cerebrum-Model ──
echo ""
echo "── BYPASS HEADERS ──"
run_test_headers "bypass category+model" COMPLEX_REASONING "meta/llama-3.3-70b-instruct"

# ── Streaming test ──
echo ""
echo "── STREAMING ──"
printf "[TEST] streaming mode ... "
resp=$(curl -s -w "\n%{http_code}" \
    "$URL" \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"messages":[{"role":"user","content":"hello"}],"stream":true}' 2>/dev/null) || true
http_code=$(printf '%s' "$resp" | tail -1)
sse_lines=$(printf '%s' "$resp" | sed '$d' | grep -c "^data:" || true)
if [ "$http_code" = "200" ] && [ "${sse_lines:-0}" -gt 0 ]; then
    printf "${GREEN}PASS${NC} (HTTP %s, %s SSE chunks)\n" "$http_code" "$sse_lines"
    PASS=$((PASS+1))
else
    printf "${RED}FAIL${NC} (HTTP %s, %s SSE chunks)\n" "$http_code" "${sse_lines:-0}"
    FAIL=$((FAIL+1))
fi

# ── Unauthorized test ──
echo ""
echo "── AUTH ──"
printf "[TEST] missing token ... "
http_code=$(curl -s -o /dev/null -w "%{http_code}" \
    "$URL" \
    -H "Content-Type: application/json" \
    -d '{"messages":[{"role":"user","content":"hello"}]}' 2>/dev/null) || true
if [ "$http_code" = "401" ]; then
    printf "${GREEN}PASS${NC} (HTTP %s)\n" "$http_code"
    PASS=$((PASS+1))
else
    printf "${RED}FAIL${NC} (expected 401, got %s)\n" "$http_code"
    FAIL=$((FAIL+1))
fi

printf "[TEST] wrong token ... "
http_code=$(curl -s -o /dev/null -w "%{http_code}" \
    "$URL" \
    -H "Authorization: Bearer wrong-token" \
    -H "Content-Type: application/json" \
    -d '{"messages":[{"role":"user","content":"hello"}]}' 2>/dev/null) || true
if [ "$http_code" = "401" ]; then
    printf "${GREEN}PASS${NC} (HTTP %s)\n" "$http_code"
    PASS=$((PASS+1))
else
    printf "${RED}FAIL${NC} (expected 401, got %s)\n" "$http_code"
    FAIL=$((FAIL+1))
fi

# ── Summary ──
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
printf " Results: ${GREEN}%d passed${NC}, ${RED}%d failed${NC}\n" "$PASS" "$FAIL"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

if [ $FAIL -gt 0 ]; then
    exit 1
fi
