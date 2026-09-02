# Authentication

The control API requires authentication for all endpoints under `/api/v1`. The gateway supports session cookies, a bootstrap bearer, and CSRF protection.

## OIDC sign-in

When OIDC is configured, select **Continue with SSO** on the login page. The gateway redirects the browser to the configured provider.

The gateway uses authorization code flow with PKCE. It validates state, nonce, issuer, audience, token signature, and the provider JWKS.

The gateway accepts an OIDC identity only when its subject or verified email matches the configured allowlist.

Use the local `operator` password when the provider is unavailable or during recovery.

## Session cookie

First, request the login CSRF cookie:

```bash
curl -c cookies.txt http://localhost:8080/api/v1/auth/status
```

Read the `iptv_csrf` value from `cookies.txt`.

Start a session with the login endpoint:

```bash
curl -b cookies.txt -c cookies.txt -X POST http://localhost:8080/api/v1/auth/login \
  -H "Content-Type: application/json" \
  -H "x-csrf-token: <csrf-cookie-value>" \
  -d '{"username": "operator", "password": "your-admin-password"}'
```

The response sets the `iptv_session` and `iptv_csrf` cookies. The session expires after 12 hours.

Check the session status:

```bash
curl -b cookies.txt http://localhost:8080/api/v1/auth/status
```

Read the new `iptv_csrf` value from `cookies.txt`.

End the session:

```bash
curl -b cookies.txt -X POST http://localhost:8080/api/v1/auth/logout \
  -H "Content-Type: application/json" \
  -H "x-csrf-token: <csrf-cookie-value>" \
  -d '{}'
```

## Bearer token

Use the bootstrap bearer token only before the first password sign-in:

```bash
curl -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  http://localhost:8080/api/v1/system
```

The bootstrap token is the value of `IPTV_ADMIN_BOOTSTRAP_TOKEN`.

The bootstrap token grants full administrator access before the first password sign-in.

The first successful password sign-in disables the bootstrap token. A restart does not enable the token again.

Do not put the bootstrap token in a URL. Do not use it for routine automation.

The current release does not provide general API tokens. Use a session cookie for later API access.

## CSRF protection

Mutating requests require a CSRF token. The login response sets the `iptv_csrf` cookie. Send the cookie value in the `x-csrf-token` header for POST, PUT, PATCH, and DELETE requests.

```bash
curl -b cookies.txt -X POST http://localhost:8080/api/v1/sources \
  -H "Content-Type: application/json" \
  -H "x-csrf-token: <csrf-cookie-value>" \
  -d '{"type": "m3u", "name": "test", "url": "https://example.com/playlist.m3u"}'
```

Bootstrap bearer requests do not require a CSRF header.

## Admin account

The default admin username is `operator`. The password comes from `IPTV_ADMIN_PASSWORD` or the hash in `IPTV_ADMIN_PASSWORD_HASH`.

When `IPTV_ADMIN_PASSWORD_HASH` is set, the core service ignores the plaintext password. Use an Argon2id PHC-formatted hash in production.

## Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/auth/status` | Return the current session status. |
| POST | `/api/v1/auth/login` | Start an admin session. |
| POST | `/api/v1/auth/logout` | End the current session. |
| GET | `/api/v1/auth/oidc/start` | Start OIDC sign-in and redirect to the provider. |
| GET | `/api/v1/auth/oidc/callback` | Validate the provider response and create a session. |
