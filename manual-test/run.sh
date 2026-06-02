#!/bin/sh
set -eu

# ── Config ──
HOST="${HOST:-localhost:10000}"
URL="http://${HOST}/v1/chat/completions"
TOKEN="${PROXY_API_BEARER_TOKEN:-}"

if [ -z "$TOKEN" ]; then
    echo "ERROR: PROXY_API_BEARER_TOKEN is not set" >&2
    exit 1
fi

# ── Colors ──
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
    # strict=False allows control characters inside string values
    # (NVIDIA 70B occasionally returns raw newlines in content)
    d = json.load(sys.stdin, strict=False)
    print(d.get('model', d.get('upstream_model', '')))
except Exception as e:
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
    wait "$_curl_pid" 2>/dev/null
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
    wait "$_curl_pid" 2>/dev/null
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

echo "============================================"
echo " Cerebrum Manual Route Tests"
echo " Target: $URL"
echo "============================================"
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

# ── FALLBACK (expects NVIDIA nvidia/nemotron-3-nano-30b-a3b) ──
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
echo "============================================"
printf " Results: ${GREEN}%d passed${NC}, ${RED}%d failed${NC}\n" "$PASS" "$FAIL"
echo "============================================"
if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
