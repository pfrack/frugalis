#!/usr/bin/env bash
set -euo pipefail

# Guard: ensure constant_time_eq_str is used (not ==) for all auth comparisons.
# Called by `make guard-auth` in CI.

AUTH_FILES=$(find src/routing -name 'auth*' -type f 2>/dev/null || true)

if [ -z "$AUTH_FILES" ]; then
  echo "FAIL: no auth files found in src/routing/"
  exit 1
fi

# 1. Presence check — constant_time_eq_str must be called at least once
if ! grep -rn 'constant_time_eq_str' $AUTH_FILES > /dev/null 2>&1; then
  echo "FAIL: constant_time_eq_str not found in src/routing/auth* — function may have been removed"
  exit 1
fi

# 2. Forbidden-pattern check — == must not appear in auth comparison context
VIOLATIONS=""
for f in $AUTH_FILES; do
  # Skip test modules and comments
  matches=$(grep -n '==' "$f" \
    | grep -v 'constant_time_eq_str' \
    | grep -v '#\[cfg(test)]' \
    | grep -v 'mod tests' \
    | grep -v '^\s*//' \
    | grep -v '^\s*#' \
    || true)

  if [ -n "$matches" ]; then
    while IFS= read -r line; do
      lineno=$(echo "$line" | cut -d: -f1)
      content=$(echo "$line" | cut -d: -f2-)

      # Check if line references a secret field or credential comparison
      if echo "$content" | grep -qE 'proxy_api_bearer_token|dashboard_basic_user|dashboard_basic_password'; then
        VIOLATIONS="${VIOLATIONS}${f}:${lineno}: ${content}\n"
      fi
    done <<< "$matches"
  fi
done

if [ -n "$VIOLATIONS" ]; then
  echo "FAIL: == found in auth comparison context (must use constant_time_eq_str):"
  echo -e "$VIOLATIONS"
  exit 1
fi

echo "OK: all auth comparisons use constant_time_eq_str"
exit 0
