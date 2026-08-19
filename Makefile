.PHONY: doctor fmt lint audit test test-rust test-web test-integration postgres-test coverage coverage-rust coverage-web coverage-changed fuzz-smoke ci build compose-config compose-up compose-down media-acceptance fault-acceptance live-acceptance

IPTV_TEST_DATABASE_URL ?= postgres://iptv:iptv-development@127.0.0.1:54329/iptv

doctor:
	./scripts/doctor.sh

fmt:
	cargo fmt --all --check

lint:
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	pnpm --dir apps/web run typecheck
	pnpm --dir apps/web run lint

audit:
	cargo deny check

test-rust:
	cargo test --workspace --all-features --all-targets

postgres-test:
	docker-compose up -d --wait postgres

test-integration: postgres-test
	IPTV_TEST_DATABASE_URL=$(IPTV_TEST_DATABASE_URL) cargo test --workspace --all-features --all-targets

test-web:
	pnpm --dir apps/web test

test: test-rust test-web

coverage-rust: postgres-test
	IPTV_TEST_DATABASE_URL=$(IPTV_TEST_DATABASE_URL) cargo llvm-cov --workspace --all-features --all-targets --ignore-filename-regex 'apps/server/src/bin/(test-provider|media-acceptance)\.rs' --fail-under-lines 86 --fail-under-functions 86 --fail-under-regions 86

coverage-web:
	pnpm --dir apps/web run coverage

coverage: coverage-rust coverage-web

coverage-changed: postgres-test
	IPTV_TEST_DATABASE_URL=$(IPTV_TEST_DATABASE_URL) ./scripts/changed-line-coverage.sh 80 HEAD~1

fuzz-smoke:
	cargo +nightly fuzz run m3u -- -max_total_time=60
	cargo +nightly fuzz run xmltv -- -max_total_time=60
	cargo +nightly fuzz run xtream -- -max_total_time=60
	cargo +nightly fuzz run events -- -max_total_time=60

ci: fmt lint audit coverage compose-config

build:
	cargo build --workspace --release
	pnpm --dir apps/web run build

compose-config:
	docker-compose config --quiet
	docker-compose --profile test config --quiet

compose-up:
	docker-compose up --build

compose-down:
	docker-compose down

media-acceptance:
	docker-compose --profile test up --build --abort-on-container-exit --exit-code-from media-acceptance media-acceptance
	docker-compose --profile test stop fake-provider

fault-acceptance:
	docker-compose --profile test up --build --abort-on-container-exit --exit-code-from fault-acceptance fault-acceptance
	docker-compose --profile test stop fake-provider toxiproxy

live-acceptance:
	docker-compose --profile test up --build --abort-on-container-exit --exit-code-from live-acceptance live-acceptance
