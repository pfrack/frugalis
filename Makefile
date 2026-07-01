# Makefile — CI gate sequences (single source of truth for CI workflows)
# Local dev: use `just` recipes. CI: use `make ci` / `make ci-deploy`.

.PHONY: ci ci-deploy fmt-check lint-strict test test-slow guard-auth build-release

# ── CI composite targets ──────────────────────────────────────────────

# PR gate: full sequence
ci: fmt-check lint-strict test test-slow guard-auth build-release

# Deploy gate: no lint/typecheck (redundant with upstream PR CI)
ci-deploy: fmt-check test test-slow guard-auth build-release

# ── Individual gates ──────────────────────────────────────────────────

fmt-check:
	cargo fmt --check

lint-strict:
	SQLX_OFFLINE=true cargo clippy --all-targets -- -D warnings

test:
	SQLX_OFFLINE=true cargo test auth
	SQLX_OFFLINE=true cargo test routes_auth
	@if [ -n "$${DATABASE_URL:-}" ]; then \
		SQLX_OFFLINE=true cargo test persistence::tests; \
		sqlx migrate run; \
		cargo test persistence_integration; \
	else \
		SQLX_OFFLINE=true cargo test persistence::tests -- --skip test_pg_log_concurrency_limit_parsed_from_env --skip test_db_connection_retry_panics_after_failures; \
		echo "DATABASE_URL not set — skipping PG integration tests"; \
	fi

test-slow:
	cargo test slow_tests -- --test-threads=1

guard-auth:
	bash .github/scripts/guard-auth-compare.sh

build-release:
	SQLX_OFFLINE=true cargo build --release
