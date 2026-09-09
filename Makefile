.PHONY: doctor fmt lint audit test test-rust test-web test-integration test-e2e test-e2e-ci test-coverage-script test-runner test-openapi-drift-check test-caddy-security test-release-compose postgres-test coverage coverage-rust coverage-web coverage-changed openapi-snapshot openapi-drift-check fuzz-smoke docs-build compose-health compose-e2e-ci ci build compose-config compose-release-config compose-up compose-down compose-dev-up compose-dev-down compose-dev-logs compose-dev-build media-acceptance fault-acceptance live-acceptance provider-stream-compare test-provider-stream-compare test-live-api-acceptance jellyfin-acceptance scale-gate scale-gate-build scale-gate-smoke scale-gate-pinned clean-debug dev

DEV_COMPOSE = docker-compose --parallel 1 -f docker-compose.yml -f docker-compose.dev.yml

IPTV_TEST_DATABASE_URL ?= postgres://iptv:iptv-development@127.0.0.1:54329/iptv
COVERAGE_CHANGED_THRESHOLD ?= 95
COVERAGE_BASE_REF ?=

doctor:
	./scripts/doctor.sh

fmt:
	cargo fmt --all --check

lint:
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	cd apps/web && CI=true pnpm run typecheck
	cd apps/web && CI=true pnpm run lint

audit:
	cargo deny check

test-rust:
	cargo test --workspace --all-features --all-targets

postgres-test:
	docker-compose --env-file .env.test up -d --wait postgres

test-integration: postgres-test
	IPTV_TEST_DATABASE_URL=$(IPTV_TEST_DATABASE_URL) cargo test --workspace --all-features --all-targets

test-web:
	cd apps/web && CI=true pnpm test

test-e2e:
	cd apps/web && CI=true pnpm run test:e2e

# Run the non-credentialed Playwright suite. This excludes the @live data
# acceptance tests and the credentialed real-source flow. The credentialed
# live-source tests stay explicit through `test-e2e-real`.
test-e2e-ci:
	cd apps/web && CI=true pnpm exec playwright test --grep-invert="@live|real-source-flow"

test-e2e-real:
	cd apps/web && CI=true IPTV_E2E_REAL_SOURCES=true pnpm run test:e2e

test: test-rust test-web

test-parallel: postgres-test
	IPTV_TEST_DATABASE_URL=$(IPTV_TEST_DATABASE_URL) cargo test --workspace --all-features --all-targets & \
	cd apps/web && CI=true pnpm test & \
	wait

coverage-rust: postgres-test
	IPTV_TEST_DATABASE_URL=$(IPTV_TEST_DATABASE_URL) cargo llvm-cov --workspace --all-features --all-targets --ignore-filename-regex 'apps/server/src/bin/(test-provider|media-acceptance|fault-acceptance|live-acceptance)\.rs' --fail-under-lines 86 --fail-under-functions 86 --fail-under-regions 86

coverage-web:
	cd apps/web && CI=true pnpm run coverage

coverage: coverage-rust coverage-web

test-coverage-script:
	./scripts/test-changed-line-coverage.sh

test-runner:
	./scripts/test-run-test-suite.sh

test-openapi-drift-check:
	./scripts/test-openapi-drift-check.sh

test-caddy-security:
	./scripts/check-caddy-security.sh

test-release-compose:
	./scripts/test-release-compose.sh

# Regenerate the checked-in OpenAPI reference snapshot from the ApiDoc.
# This target needs no database and no secrets.
openapi-snapshot:
	cargo run -p iptv-api --example print-openapi --quiet 2>/dev/null | jq -S . > tests/fixtures/openapi.json

# Compare the live /api/v1/openapi.json contract against the reference snapshot.
# Set OPENAPI_BASE_URL to a running core service (default: http://127.0.0.1:8081).
openapi-drift-check:
	./scripts/openapi-drift-check.sh

coverage-changed: postgres-test
	IPTV_TEST_DATABASE_URL=$(IPTV_TEST_DATABASE_URL) ./scripts/changed-line-coverage.sh "$(COVERAGE_CHANGED_THRESHOLD)" "$(COVERAGE_BASE_REF)"

cleanup-test-data:
	./scripts/cleanup-test-data.sh

# Remove debug and test Cargo artifacts while keeping release binaries.
clean-debug:
	cargo clean --profile dev
	cargo clean --profile test

fuzz-smoke:
	cargo +nightly fuzz run m3u -- -max_total_time=60
	cargo +nightly fuzz run xmltv -- -max_total_time=60
	cargo +nightly fuzz run xtream -- -max_total_time=60
	cargo +nightly fuzz run events -- -max_total_time=60

docs-build:
	if [ -x docs/.venv/bin/mkdocs ]; then docs/.venv/bin/mkdocs build --config-file docs/mkdocs.yml --strict --clean; else mkdocs build --config-file docs/mkdocs.yml --strict --clean; fi

# Start the production-shaped stack and wait for each required health check.
# Always remove the temporary containers after the check.
compose-health:
	trap 'docker-compose --env-file .env.test down' EXIT; \
	docker-compose --env-file .env.test up --build -d --wait postgres core web gateway

# Run Playwright against the same temporary stack used by the health check.
# Keep this target separate so developers can run Playwright against a stack.
compose-e2e-ci:
	trap 'docker-compose --env-file .env.test down' EXIT; \
	docker-compose --env-file .env.test up --build -d --wait postgres core web gateway; \
	(cd apps/web && IPTV_ADMIN_PASSWORD=test-administrator-password CI=true pnpm exec playwright test --grep-invert="@live|real-source-flow")

ci: fmt lint audit coverage test-coverage-script test-openapi-drift-check coverage-changed fuzz-smoke media-acceptance fault-acceptance compose-health docs-build compose-e2e-ci compose-config compose-release-config test-release-compose

build:
	cargo build --workspace --release
	cd apps/web && CI=true pnpm run build

compose-config:
	docker-compose --env-file .env.example config --quiet
	docker-compose --env-file .env.test --profile test config --quiet
	$(DEV_COMPOSE) --env-file .env.example config --quiet

compose-release-config:
	IPTV_VERSION=latest docker-compose -f docker-compose.appliance.yml --env-file .env.appliance.example config --quiet

compose-up:
	docker-compose up --build

compose-down:
	docker-compose down

compose-dev-build:
	$(DEV_COMPOSE) build core web

compose-dev-up:
	$(DEV_COMPOSE) up --no-build

compose-dev-down:
	$(DEV_COMPOSE) down

compose-dev-logs:
	$(DEV_COMPOSE) logs -f core worker web

media-acceptance:
	trap 'docker-compose --env-file .env.test --profile test down --remove-orphans' EXIT; \
	docker-compose --env-file .env.test build core; \
	docker-compose --env-file .env.test --profile test up --abort-on-container-exit --exit-code-from media-acceptance media-acceptance

fault-acceptance:
	trap 'docker-compose --env-file .env.test --profile test down --remove-orphans' EXIT; \
	docker-compose --env-file .env.test build core; \
	docker-compose --env-file .env.test --profile test up --abort-on-container-exit --exit-code-from fault-acceptance fault-acceptance

live-acceptance:
	python3 scripts/live-api-acceptance.py --env-file .env.live --provider-env-file .env.xtreme

provider-stream-compare:
	python3 scripts/provider-stream-compare.py

test-provider-stream-compare:
	python3 scripts/provider-stream-compare.py --self-test

test-live-api-acceptance:
	python3 scripts/live-api-acceptance.py --self-test
	python3 scripts/test-live-api-acceptance.py

jellyfin-acceptance:
	trap 'IPTV_PUBLIC_BASE_URL=http://gateway:8080 docker-compose --project-name iptv-jellyfin-acceptance --env-file .env.test --profile test down --volumes --remove-orphans' EXIT; \
	IPTV_PUBLIC_BASE_URL=http://gateway:8080 docker-compose --project-name iptv-jellyfin-acceptance --env-file .env.test build core; \
	IPTV_PUBLIC_BASE_URL=http://gateway:8080 docker-compose --project-name iptv-jellyfin-acceptance --env-file .env.test --profile test up --abort-on-container-exit --exit-code-from jellyfin-acceptance jellyfin-acceptance

live-acceptance-gateway:
	trap 'IPTV_GATEWAY_PORT=18080 IPTV_POSTGRES_PORT=54339 docker-compose --project-name iptv-live-acceptance --env-file .env.test down --volumes --remove-orphans' EXIT; \
	IPTV_GATEWAY_PORT=18080 IPTV_POSTGRES_PORT=54339 docker-compose --project-name iptv-live-acceptance --env-file .env.test build core; \
	IPTV_GATEWAY_PORT=18080 IPTV_POSTGRES_PORT=54339 docker-compose --project-name iptv-live-acceptance --env-file .env.test --profile test up --abort-on-container-exit --exit-code-from live-acceptance-gateway live-acceptance-gateway

scale-gate-build:
	cargo build --release -p iptv-gateway --features scale-gate --bin scale-gate

# Run the deterministic scale gate on the host. Activation is measured only
# when IPTV_TEST_DATABASE_URL is set. Generated artifacts stay in a temp dir.
scale-gate: scale-gate-build
	./target/release/scale-gate

# Quick deterministic smoke run with a tiny workload. Use it to verify the
# gate runner logic without the full 1.16-million-entry fixture.
scale-gate-smoke: scale-gate-build
	SCALE_GATE_ENTRIES=1000 SCALE_GATE_CHANNELS=10 SCALE_GATE_PROGRAMMES=200 ./target/release/scale-gate

# Run the deterministic scale gate on the pinned runner container. The
# container activates each snapshot against the test PostgreSQL instance.
scale-gate-pinned:
	docker-compose --env-file .env.test build core
	docker-compose --env-file .env.test --profile test up --abort-on-container-exit --exit-code-from scale-gate scale-gate

dev:
	docker-compose up -d postgres web
	docker run -d --name iptv-gateway-caddy --network iptv-gateway_default -p 8080:8080 \
		--add-host=host.docker.internal:host-gateway \
		-v $(PWD)/deploy/Caddyfile.local:/etc/caddy/Caddyfile:ro \
		caddy:2.10-alpine
	@echo "Development stack started at http://localhost:8080"
	@echo "Start the API and worker locally with:"
	@echo "  ./target/release/iptv-gateway serve &"
	@echo "  ./target/release/iptv-gateway worker &"
