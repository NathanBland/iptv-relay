# Local Setup

This guide sets up a local development environment without Docker for the core service.

## Prerequisites

Install these tools:

- Rust 1.97.1.
- Node 24 and pnpm 11.
- FFmpeg and VLC CLI for live adapter tests.
- `cargo-llvm-cov` for the Rust coverage gate.
- Docker and Docker Compose for PostgreSQL.

## Verify the environment

Run the doctor check:

```bash
make doctor
```

The doctor script verifies the installed tools and versions.

## Start PostgreSQL

Start PostgreSQL with Compose:

```bash
make postgres-test
```

PostgreSQL listens on `127.0.0.1:54329`.

## Set environment variables

Copy `.env.example` to `.env`. Set the test database URL:

```bash
export IPTV_TEST_DATABASE_URL=postgres://iptv:iptv-development@127.0.0.1:54329/iptv
```

Set the core service variables for local runs:

```bash
export DATABASE_URL=postgres://iptv:iptv-development@127.0.0.1:54329/iptv
export IPTV_OUTPUT_TOKEN=$(openssl rand -hex 32)
export IPTV_ADMIN_BOOTSTRAP_TOKEN=$(openssl rand -hex 32)
export IPTV_ADMIN_PASSWORD=$(openssl rand -base64 24)
export IPTV_MASTER_KEY=$(openssl rand -base64 32)
```

## Run the core service

```bash
cargo run -p iptv-gateway -- serve
```

The core service listens on `0.0.0.0:8081`.

## Run the worker

```bash
cargo run -p iptv-gateway -- worker
```

## Run the web app

Install web dependencies:

```bash
pnpm --dir apps/web install
```

Start the Vite dev server:

```bash
pnpm --dir apps/web dev
```

The web app listens on port 5173 by default. Set `CORE_INTERNAL_URL` to `http://127.0.0.1:8081` for the dev server.

## Build

Build the Rust workspace:

```bash
make build
```

The build target compiles the release binary and the web bundle.

## Docker dev mode

For hot reload, use the Docker dev environment:

```bash
make compose-dev-build
make compose-dev-up
```

See [Docker Compose](../configuration/docker-compose.md#dev-mode) for details.

## Preview documentation

Create the documentation environment:

```bash
python3 -m venv docs/.venv
docs/.venv/bin/python -m pip install -r docs/requirements.txt
```

Start the local preview:

```bash
docs/.venv/bin/mkdocs serve --config-file docs/mkdocs.yml
```

Open `http://127.0.0.1:8000` in a browser. The preview reloads after a documentation file changes.

Build the strict site from the repository root:

```bash
make docs-build
./scripts/check-doc-links.sh
```

The generated `docs/site/` directory is local output. Git ignores this directory.

## Publish documentation

The `Deploy docs` GitHub Actions workflow builds the strict site and publishes it to GitHub Pages.

The default site address is `https://nathanbland.github.io/iptv-relay/`.

### Custom domain

A custom domain needs an external DNS record. The repository cannot create or change this record.

1. Add a CNAME record at your DNS provider.
2. Point the record to `nathanbland.github.io`.
3. Set the custom domain in the GitHub Pages settings for the repository.

The DNS change is an external task. The documentation build does not depend on it.

### Verify the published site

Wait for the `Deploy docs` workflow to complete. Then run this command:

```bash
curl --fail --head https://nathanbland.github.io/iptv-relay/
```

For a custom domain, replace the address with your domain:

```bash
curl --fail --head https://docs.example.com/
dig docs.example.com CNAME
```

A successful response returns the HTTP status `200`.
