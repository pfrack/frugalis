# justfile — single source of truth for build/test/lint/run commands
# Run `just` to list all recipes.

set dotenv-load
set shell := ["bash", "-euo", "pipefail", "-c"]

# List all recipes (default goal)
default:
    @just --list

# Diagnose missing env vars
print-tokens:
    @echo "PROXY_API_BEARER_TOKEN=$${PROXY_API_BEARER_TOKEN:-(not set)}"
    @echo "DASHBOARD_BASIC_USER=$${DASHBOARD_BASIC_USER:-(not set)}"
    @echo "DASHBOARD_BASIC_PASSWORD=$${DASHBOARD_BASIC_PASSWORD:-(not set)}"
    @echo "DATABASE_URL=$${DATABASE_URL:-(not set)}"
    @echo "OTEL_ENABLED=$${OTEL_ENABLED:-(not set)}"
    @echo "OTEL_EXPORTER_OTLP_ENDPOINT=$${OTEL_EXPORTER_OTLP_ENDPOINT:-(not set)}"

# ── Format ────────────────────────────────────────────────────────────

# Run cargo fmt
fmt:
    cargo fmt

# Check formatting (gate form)
fmt-check:
    cargo fmt --check

# ── Lint ──────────────────────────────────────────────────────────────

# Run clippy (advisory)
lint:
    cargo clippy --all-targets

# Run clippy (gate form: warnings are errors)
lint-strict:
    cargo clippy --all-targets -- -D warnings

# Run clippy with otel feature
lint-otel:
    cargo clippy --features otel --all-targets -- -D warnings

# ── Type-check ────────────────────────────────────────────────────────

# Run cargo check
check:
    cargo check

# Run cargo check with otel feature
check-otel:
    cargo check --features otel

# ── Build ─────────────────────────────────────────────────────────────

# Build debug binary
build:
    cargo build

# Build debug binary with otel feature
build-otel:
    cargo build --features otel

# Build release binary (hermetic, locked)
build-release:
    cargo build --release --locked

# Build release binary with otel feature
build-release-otel:
    cargo build --release --locked --features otel

# ── Test (fast) ───────────────────────────────────────────────────────

# Run tests (forwards args: just test auth)
test *ARGS:
    cargo test {{quote(ARGS)}}

# Run tests with otel feature
test-otel:
    cargo test --features otel

# Run auth tests
test-auth:
    cargo test auth

# Run route auth tests
test-routes-auth:
    cargo test routes_auth

# Run persistence unit tests (no DB required)
test-persistence:
    cargo test persistence::tests

# ── Test (DB integration) ─────────────────────────────────────────────

# Run persistence integration tests (starts postgres via compose)
test-persistence-integration:
    docker compose up -d postgres
    cargo test persistence_integration

# Run slow tests (single-threaded)
test-slow:
    cargo test slow_tests -- --test-threads=1

# ── Test (focused/phase) ─────────────────────────────────────────────

# Run chain tests
test-chain:
    cargo test chain

# Run snippet tests
test-snippet:
    cargo test snippet

# Run format_sse tests
test-format-sse:
    cargo test format_sse

# Run streaming_error tests
test-streaming-error:
    cargo test streaming_error

# Run json_shape tests
test-json-shape:
    cargo test json_shape

# ── DB / migration ───────────────────────────────────────────────────

# Run sqlx migrations (redundancy check; app auto-migrates at startup)
migrate:
    sqlx migrate run

# Show migration status
migrate-status:
    sqlx migrate info

# Revert last migration
migrate-revert:
    sqlx migrate revert

# ── Run ───────────────────────────────────────────────────────────────

# Run frugalis with sensible dev defaults
run:
    RUST_LOG=info cargo run

# Run frugalis with OTel enabled (requires otel-collector via compose)
run-otel:
    RUST_LOG=info OTEL_ENABLED=true OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 cargo run --features otel

# Run frugalis release binary
run-release:
    RUST_LOG=info cargo run --release

# Run frugalis release binary with OTel
run-otel-release:
    RUST_LOG=info OTEL_ENABLED=true OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 cargo run --release --features otel

# Validate config.toml
validate-config:
    cargo run -- --validate

# ── Manual / harness ─────────────────────────────────────────────────

# Run manual test harness (interactive)
manual:
    manual-test/run.sh

# Run basic manual tests
manual-basic:
    scripts/manual_tests.sh --basic

# Run manual tests in auto mode
manual-auto:
    manual-test/run.sh --auto

# Run persistence manual tests (3-tier: memory/sqlite/postgres)
manual-persistence:
    manual-test/run.sh --persistence

# Run fewshot manual tests
manual-fewshot:
    manual-test/run.sh --fewshot

# ── CI composite ─────────────────────────────────────────────────────

# Mirror deploy.yml gate sequence: fmt-check, lint-strict, test, test-slow, build-release
ci: fmt-check lint-strict test test-slow build-release

# CI + otel lint and tests
ci-otel: ci lint-otel test-otel

# Verification gates: test + slow + lint + fmt
gates: test test-slow lint-strict fmt-check

# ── Clean ─────────────────────────────────────────────────────────────

# Clean build artifacts
clean:
    cargo clean

# Clean build artifacts and temp files
clean-all:
    cargo clean
    rm -f /tmp/frugalis-config-*.toml /tmp/frugalis-config-*.yaml /tmp/frugalis-test-*.log /tmp/frugalis_test_*.db /tmp/frugalis.db
    rm -rf /tmp/frugalis-patterns/ /tmp/fewshot_int_*.yaml

# ── OTel compose helpers ─────────────────────────────────────────────

# Start OTel collector (and postgres)
otel-up:
    docker compose --profile otel up -d

# Stop OTel collector
otel-down:
    docker compose --profile otel down

# Follow OTel collector logs
otel-logs:
    docker compose logs -f otel-collector

# Start compose services (postgres + app by default; add --profile otel for collector)
compose-up:
    docker compose --profile app up -d

# Stop all compose services
compose-down:
    docker compose down
