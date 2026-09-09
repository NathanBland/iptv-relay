# Environment Variables

The gateway reads configuration from environment variables. The Compose file passes these variables to the `core` and `worker` services.

## Core service

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | Generated | Compose creates this value from `POSTGRES_PASSWORD`. |
| `IPTV_GATEWAY_BIND` | `127.0.0.1` | Host address for the gateway port. |
| `IPTV_GATEWAY_PORT` | `8080` | Host port for the gateway. |
| `IPTV_BIND` | `0.0.0.0:8081` | Core service bind address. |
| `IPTV_PUBLIC_BASE_URL` | `http://localhost:8080` | Public gateway base URL for output endpoints. |
| `IPTV_OUTPUT_TOKEN` | Required | Random 256-bit token that protects output endpoints. |
| `IPTV_ADMIN_BOOTSTRAP_TOKEN` | Required | Random 256-bit bearer token for bootstrap admin access. |
| `IPTV_ADMIN_PASSWORD` | Required without a hash | Password with at least 12 characters for the `operator` account. |
| `IPTV_ADMIN_PASSWORD_HASH` | _empty_ | Argon2id PHC hash. When set, the plaintext password is ignored. |
| `IPTV_MASTER_KEY` | Required | Base64 32-byte key for secret encryption. |
| `IPTV_TUNER_COUNT` | `1` | HDHomeRun tuner count for the environment output profile. |
| `IPTV_DEV_MODE` | `false` | Enable local development behavior. Keep this false for deployments. |
| `IPTV_DEV_AUTH_DISABLED` | `false` | Disable web and control API authentication when `IPTV_DEV_MODE=true`. |
| `RUST_LOG` | `info` | Log level filter. |

### Optional OIDC sign-in

The gateway enables OIDC when the issuer and client ID are set. Set at least one approved subject or verified email.

| Variable | Default | Description |
|----------|---------|-------------|
| `IPTV_OIDC_ISSUER_URL` | _empty_ | HTTPS issuer URL. HTTP is valid only for a loopback development issuer. |
| `IPTV_OIDC_CLIENT_ID` | _empty_ | OIDC client identifier. |
| `IPTV_OIDC_CLIENT_SECRET` | _empty_ | Confidential-client secret. Leave empty for a public PKCE client. |
| `IPTV_OIDC_REDIRECT_URL` | Derived | Callback URL. The default uses `IPTV_PUBLIC_BASE_URL`. |
| `IPTV_OIDC_ALLOWED_SUBJECTS` | _empty_ | Comma or space separated approved subject values. |
| `IPTV_OIDC_ALLOWED_EMAILS` | _empty_ | Comma or space separated approved verified email values. |
| `IPTV_OIDC_SCOPES` | `openid profile email` | Space separated scopes. When empty, the gateway uses `openid profile email`. Include `openid` for standard OpenID Connect claims. |

The login page shows the OIDC option only when the gateway reports OIDC as enabled.

The callback uses authorization code flow with PKCE, state, and nonce. The gateway validates the issuer, audience, signature, and JWKS key.

Keep the local `operator` password configured for break-glass access.

## Worker service

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | Generated | Compose creates this value from `POSTGRES_PASSWORD`. |
| `IPTV_MASTER_KEY` | Required | Base64 32-byte key for secret encryption. |
| `IPTV_WORKER_COUNT` | `4` | Number of worker replicas. |
| `IPTV_WORKER_ID` | _derived from hostname_ | Override the worker ID. |
| `RUST_LOG` | `info` | Log level filter. |

Each worker replica derives its worker ID from the container hostname when `IPTV_WORKER_ID` is not set.

## Web service

| Variable | Default | Description |
|----------|---------|-------------|
| `CORE_INTERNAL_URL` | `http://core:8081` | Internal core service URL. |
| `PORT` | `3000` | Web server port. |

## PostgreSQL

| Variable | Default | Description |
|----------|---------|-------------|
| `POSTGRES_DB` | `iptv` | Database name. |
| `POSTGRES_USER` | `iptv` | Database user. |
| `POSTGRES_PASSWORD` | Required | Database password. |
| `IPTV_POSTGRES_PORT` | `54329` | Host port mapped to PostgreSQL. |

## Optional test inputs

| Variable | Default | Description |
|----------|---------|-------------|
| `IPTV_TEST_M3U_URL` | _empty_ | M3U URL for the live-provider acceptance test. |
| `IPTV_TEST_XMLTV_URL` | _empty_ | XMLTV URL for the live-provider acceptance test. |
| `IPTV_TEST_PROVIDER_MAX_CONNECTIONS` | `3` | Maximum connections for the test provider. |
| `IPTV_TEST_TARGETS` | _empty_ | Pipe-separated provider display names for stream comparison. |

!!! warning
    Never commit real values for test inputs. Put live-provider URLs only in environment variables.

## Security notes

Replace every placeholder credential before you start the service.

The base Compose configuration binds the gateway to `127.0.0.1`.

If you expose the gateway on a LAN, put it behind a TLS reverse proxy.

Prefer `IPTV_ADMIN_PASSWORD_HASH` over `IPTV_ADMIN_PASSWORD` in production. Use an Argon2id PHC-formatted hash.

Generate a random 32-byte base64 key for `IPTV_MASTER_KEY`. The core and worker services must use the same key.

The output token protects all `/out/{token}/...` endpoints. Use a random 256-bit value.

### Local development authentication

Set both development variables to bypass web and control API login during local testing:

```text
IPTV_DEV_MODE=true
IPTV_DEV_AUTH_DISABLED=true
```

The gateway rejects `IPTV_DEV_AUTH_DISABLED=true` when `IPTV_DEV_MODE` is false.

This mode does not bypass output-token or internal worker authentication.

Use this mode only with the local development Compose override.
