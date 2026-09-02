.PHONY: doctor fmt lint audit test test-rust test-web test-integration test-e2e test-coverage-script postgres-test coverage coverage-rust coverage-web coverage-changed fuzz-smoke ci build compose-config compose-up compose-down compose-dev-up compose-dev-down compose-dev-logs compose-dev-build media-acceptance fault-acceptance live-acceptance scale-gate scale-gate-build scale-gate-smoke scale-gate-pinned dev

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

coverage-changed: postgres-test
	IPTV_TEST_DATABASE_URL=$(IPTV_TEST_DATABASE_URL) ./scripts/changed-line-coverage.sh "$(COVERAGE_CHANGED_THRESHOLD)" "$(COVERAGE_BASE_REF)"

cleanup-test-data:
	./scripts/cleanup-test-data.sh

fuzz-smoke:
	cargo +nightly fuzz run m3u -- -max_total_time=60
	cargo +nightly fuzz run xmltv -- -max_total_time=60
	cargo +nightly fuzz run xtream -- -max_total_time=60
	cargo +nightly fuzz run events -- -max_total_time=60

ci: fmt lint audit coverage test-coverage-script coverage-changed test-e2e-ci compose-config

build:
	cargo build --workspace --release
	cd apps/web && CI=true pnpm run build

compose-config:
	docker-compose --env-file .env.example config --quiet
	docker-compose --env-file .env.test --profile test config --quiet
	$(DEV_COMPOSE) --env-file .env.example config --quiet

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
	docker-compose --env-file .env.test build core
	docker-compose --env-file .env.test --profile test up --abort-on-container-exit --exit-code-from media-acceptance media-acceptance
	docker-compose --env-file .env.test --profile test stop fake-provider

fault-acceptance:
	docker-compose --env-file .env.test build core
	docker-compose --env-file .env.test --profile test up --abort-on-container-exit --exit-code-from fault-acceptance fault-acceptance
	docker-compose --env-file .env.test --profile test stop fake-provider toxiproxy

live-acceptance:
	docker-compose --env-file .env.test build core
	docker-compose --env-file .env.test --profile test up --abort-on-container-exit --exit-code-from live-acceptance live-acceptance

live-acceptance-gateway:
	docker-compose --env-file .env.test build core
	docker-compose --env-file .env.test --profile test up --abort-on-container-exit --exit-code-from live-acceptance-gateway live-acceptance-gateway
	docker-compose --env-file .env.test --profile test stop fake-provider
	docker-compose --env-file .env.test --profile test rm -f live-acceptance-gateway

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
