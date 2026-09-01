# Web API contract

The production web client uses same-origin relative URLs. All successful responses use `Content-Type: application/json`. All timestamps are RFC 3339 strings with an explicit offset, preferably UTC `Z`. Unknown object fields may be added, but the fields below are required.

## Errors

Non-2xx responses use RFC 9457 `Content-Type: application/problem+json`:

```json
{
  "type": "urn:iptv:error:source-conflict",
  "title": "Source already exists",
  "status": 409,
  "detail": "A source with that endpoint is already configured.",
  "instance": "/api/v1/sources"
}
```

The HTTP status is authoritative; the client does not trust a conflicting `status` value in the body. `detail` and `instance` are optional.

Protected endpoints return `401 Unauthorized` when the session is missing or expired. The browser redirects a protected `401` to `/login?next=<same-origin-path>` once per client instance. Authentication endpoints never trigger that redirect, and `/login` is rejected as a return target to prevent redirect loops.

## Authentication and CSRF

The server owns two same-origin cookies:

- `iptv_session` contains the opaque session identifier. Set it with `HttpOnly`, `Secure` on HTTPS, `SameSite=Lax` (or stricter), and `Path=/`. JavaScript must never be able to read it.
- `iptv_csrf` contains a non-secret CSRF token associated with the browser/session. Set it with `Secure` on HTTPS, `SameSite=Lax` (or stricter), and `Path=/`, but without `HttpOnly` because the web client copies its value to the request header.

Every `POST`, `PUT`, `PATCH`, and `DELETE` request sends `X-CSRF-Token: <iptv_csrf cookie value>`. The server must compare the cookie, header, and server-side session binding using a constant-time comparison. Rotate the CSRF value when authentication state changes. Reject a missing or mismatched value with an RFC 9457 `403 Forbidden` response.

### GET `/api/v1/auth/status`

This endpoint is intentionally public and always returns `200 OK`. It must ensure an `iptv_csrf` cookie exists, including before login, so the subsequent login mutation can pass CSRF validation.

Signed out:

```json
{ "authenticated": false }
```

Signed in:

```json
{
  "authenticated": true,
  "user": {
    "id": "operator-1",
    "username": "operator",
    "displayName": "Relay operator"
  }
}
```

### POST `/api/v1/auth/login`

Requires the CSRF header. Request:

```json
{
  "username": "operator",
  "password": "correct horse battery staple"
}
```

On success, return `200 OK` with the signed-in auth-status shape above, create or rotate `iptv_session`, and rotate `iptv_csrf`. Invalid credentials return a generic RFC 9457 `401` that does not reveal whether the username exists.

### POST `/api/v1/auth/logout`

Requires the CSRF header. The request body is `{}`. Clear the session cookie, invalidate the server-side session, rotate or clear the CSRF cookie, and return:

```json
{
  "ok": true,
  "message": "Signed out."
}
```

## GET `/api/v1/system`

```json
{
  "channels": 486,
  "healthyStreams": 932,
  "activeSessions": 6,
  "guideCoverage": 96.8,
  "providerConnections": 3,
  "providerLimit": 3
}
```

All fields are JSON numbers.

## GET `/api/v1/sources`

Returns a JSON array of source objects:

```json
[
  {
    "id": "source-prime",
    "name": "Prime IPTV",
    "kind": "Xtream",
    "state": "healthy",
    "channels": 682,
    "lastSync": "2026-08-19T17:57:00Z",
    "endpoint": "https://provider.invalid/player_api.php"
  }
]
```

`kind` is one of `M3U`, `Xtream`, `XMLTV`, or `Network tuner`. `state` is one of `healthy`, `degraded`, `offline`, or `syncing`.

## POST `/api/v1/sources`

Request:

```json
{
  "name": "Prime IPTV",
  "kind": "Xtream",
  "endpoint": "https://provider.invalid/player_api.php"
}
```

Return `201 Created` with one complete source object in the same shape used by `GET /api/v1/sources`.

The server encrypts the complete endpoint before storage, persists only a credential-free display endpoint alongside it, and transactionally enqueues a `refresh-source` job plus an audit event. The response `endpoint` is therefore redacted and may use `/…` in place of a sensitive path.

## GET `/api/v1/jobs`

Returns up to 100 most-recent jobs. Payloads, worker identifiers, and encrypted source material are never returned. `progress` and `lastError` are recursively redacted before serialization.

```json
[
  {
    "id": "019c...",
    "kind": "refresh-source",
    "status": "running",
    "progress": { "processed": 120, "total": 486 },
    "attempts": 1,
    "maxAttempts": 3,
    "lastError": null,
    "createdAt": "2026-08-19T18:00:00Z",
    "updatedAt": "2026-08-19T18:01:00Z",
    "completedAt": null
  }
]
```

## POST `/api/v1/jobs/{jobId}/cancel`

Requires authentication and CSRF like every mutation. Queued and running jobs transition durably to `cancelled` and return:

```json
{ "ok": true, "message": "Cancellation requested." }
```

An already-terminal job returns an RFC 9457 `409`; an unknown job returns `404`.

## GET `/api/v1/settings/schema`

Returns the authenticated, backend-owned configuration catalog as a JSON array. Clients use this
metadata instead of duplicating defaults or validation limits:

```json
[
  {
    "key": "media.ring.duration_seconds",
    "label": "Live ring duration",
    "description": "Keeps this many recent seconds for new or temporarily slow viewers.",
    "valueKind": "integer",
    "defaultValue": 8,
    "minimum": 1,
    "maximum": 120,
    "unit": "seconds",
    "operationalEffect": "Higher values improve jitter tolerance and increase memory and tune latency.",
    "risk": "capacity",
    "applyRequirement": "immediate",
    "providerOverridable": true,
    "groupOverridable": false
  }
]
```

`valueKind` is `boolean`, `integer`, `string`, or `choice`. Optional range, choice, and unit fields
are omitted when they do not apply.

## GET `/api/v1/channels`

Returns a JSON array:

```json
[
  {
    "id": "channel-1",
    "number": "2.1",
    "name": "KWGN Denver",
    "group": "Denver locals",
    "tvgId": "KWGN-DT.us",
    "streams": 3,
    "primaryCodec": "H.264",
    "bitrateKbps": 7800,
    "state": "healthy"
  }
]
```

## GET `/api/v1/programmes`

Returns a JSON array:

```json
[
  {
    "id": "program-1",
    "channel": "KWGN Denver",
    "title": "Colorado’s Own News at 6",
    "start": "2026-08-19T18:00:00Z",
    "end": "2026-08-19T19:00:00Z",
    "source": "North America guide",
    "confidence": 99
  }
]
```

`confidence` is a numeric percentage from 0 through 100.

## GET `/api/v1/events`

Returns a JSON array:

```json
[
  {
    "id": "event-1",
    "group": "NFL",
    "rawTitle": "NFL 08/19 1:00 PM Broncos vs Chiefs",
    "programmeTitle": "Denver Broncos vs Kansas City Chiefs",
    "channelSlot": "NFL Event 01",
    "start": "2026-08-19T19:00:00Z",
    "state": "scheduled",
    "template": "{league} {date} {time} {away} vs {home}"
  }
]
```

`state` is one of `scheduled`, `live`, `ambiguous`, or `unmatched`.

## GET `/api/v1/sessions`

Returns one redacted aggregate for each live shared upstream. It does not invent downstream client
identity or a start time, because the media actor intentionally tracks only the viewer count:

```json
[
  {
    "providerPoolId": "provider-prime",
    "sourceId": "source-kwgn-hd",
    "configuredGeneration": 4,
    "upstreamGeneration": 4,
    "state": "streaming",
    "viewerCount": 2,
    "retainedPackets": 21276,
    "capacityPackets": 42553,
    "lagEvents": 0,
    "wrapEvents": 4,
    "overwrittenPackets": 18800,
    "reconnectAttempts": 0,
    "failoverAttempts": 0,
    "failureCount": 0,
    "lastFailure": null,
    "providerCapacity": 3,
    "providerActiveSessions": 3,
    "providerHighWatermark": 3,
    "providerAvailableSlots": 0
  }
]
```

`state` is `idle`, `reserving`, `starting`, `priming`, `streaming`, `recovering`,
`failing-over`, `stopping`, or `failed`. `lastFailure`, when present, is
`upstream-ended`, `http`, `packetization`, `priming`, or `recovery-expired`. No endpoint URL,
headers, credentials, or downstream client metadata are present.

## GET `/api/v1/session-events`

Requires authentication and returns `text/event-stream`. Named `sessions` events contain the same
JSON array returned by `GET /api/v1/sessions` and are emitted immediately, then every ten seconds.
The server emits comment keepalives during idle periods, disables proxy buffering, and asks
intermediaries not to transform the stream.

```text
event: sessions
data: [{"providerPoolId":"provider-prime",...}]
```

## GET `/metrics`

Returns Prometheus text exposition format. Media metrics are process-wide aggregates with no
provider, source, channel, URL, or credential-bearing labels. Exported gauges include provider pool
capacity/usage/high-water/availability, shared sessions, viewers, aggregate ring occupancy, and
lag/wrap/overwrite/reconnect/failover/failure counters.

## GET `/api/v1/jellyfin/setup`

Requires authentication. Returns the current Jellyfin setup state. The output token stays embedded in
each URL path and is never exposed as a separate field. The response sets `Cache-Control: no-store`
because the URLs may contain the token.

When the environment token is active and its plaintext is available at startup:

```json
{
  "status": "available",
  "playlistUrl": "https://relay.example/out/output-token/playlist.m3u",
  "xmltvUrl": "https://relay.example/out/output-token/xmltv.xml",
  "hdhrDeviceUrl": "https://relay.example/out/output-token/hdhr/device.xml",
  "guideDaysMax": 30
}
```

After a rotation the plaintext token is no longer retained, so later GET responses omit the URLs:

```json
{
  "status": "regeneration-required",
  "guideDaysMax": 30
}
```

`playlistUrl`, `xmltvUrl`, and `hdhrDeviceUrl` are absolute URLs built from the configured public
base URL and the active output token. `guideDaysMax` is the backend maximum guide horizon in days.
The web client removes the `['jellyfin-setup']` query cache entry on unmount so token-bearing URLs
do not persist after navigation.

## POST `/api/v1/jellyfin/setup/rotate`

Requires authentication and CSRF. Generates a cryptographically random output token server-side,
stores only its SHA-256 hash, and keeps the previous hash valid through an overlap window. The
plaintext token is returned once as complete published URLs and is never stored in the database.

Request (all fields optional):

```json
{
  "overlapSeconds": 300
}
```

`overlapSeconds` is the window in seconds during which the previous token remains valid. It defaults
to 300 and accepts 0 through 86400.

Response (`Cache-Control: no-store`):

```json
{
  "status": "available",
  "playlistUrl": "https://relay.example/out/new-token/playlist.m3u",
  "xmltvUrl": "https://relay.example/out/new-token/xmltv.xml",
  "hdhrDeviceUrl": "https://relay.example/out/new-token/hdhr/device.xml",
  "guideDaysMax": 30
}
```

After this response, `GET /api/v1/jellyfin/setup` returns `regeneration-required` because the
plaintext token is not retained. The previous token stays valid through the overlap window so
existing Jellyfin tuners keep working during the transition.

The browser sends cookies with `credentials: same-origin`; no backend URL is accepted from client configuration. Set `VITE_USE_MOCK_API=true` only for an explicit fixture-backed development demo.
