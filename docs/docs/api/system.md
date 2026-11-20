# System API

The system API exposes runtime diagnostics and redacted support data. The system and support endpoints require admin authentication.

## Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/system` | Return system counts and runtime versions. |
| GET | `/api/v1/support/bundle` | Return a redacted support bundle. Requires admin. |
| GET | `/api/v1/support/logs` | Return recent redacted support log entries. Requires admin. |
| GET | `/api/v1/support/stream-sessions` | Return recent ended viewer stream sessions. Requires admin. |

## Read system information

```bash
curl http://localhost:8080/api/v1/system \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns a `SystemInfo` object. The object uses camelCase field names from the OpenAPI schema. The fields are `channels`, `healthyStreams`, `activeSessions`, `guideCoverage`, `providerConnections`, `providerLimit`, `uptimeSeconds`, and `versions`.

## Read a support bundle

```bash
curl http://localhost:8080/api/v1/support/bundle \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns a `SupportBundleResponse` object. The object uses camelCase field names from the OpenAPI schema. The fields are `schemaVersion`, `generatedAt`, `redaction`, `system`, `jobs`, `sessions`, `streamHealth`, and `logs`.

The bundle redacts secrets, tokens, credentials, URLs, and sensitive diagnostic fields. Treat the bundle as a starting point for a support request. Review the bundle before you share it.

## Read support log entries

```bash
curl http://localhost:8080/api/v1/support/logs \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns an array of `SupportLogEntry` objects. Each object includes `timestamp`, `level`, `message`, and `context`. The endpoint redacts secrets and sensitive values before it returns the entries.

## Read stream session history

```bash
curl http://localhost:8080/api/v1/support/stream-sessions \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns an array of `StreamSessionRecord` objects. The newest session is first.

The relay keeps the last 200 ended sessions in memory. A restart clears the list.

Each object has these fields:

- `id`: the session identifier.
- `channelId` and `providerStreamId`: the channel and its primary provider stream.
- `container`: `ts` or `mkv`.
- `slated`: `true` when the response started with a slate.
- `startedAt`, `endedAt`, and `durationMs`: the session times.
- `bytesSent`: the bytes that the relay sent to the client.
- `endReason`: why the session ended.
- `detail`: the redacted message that goes with the end reason.
- `remux`: the Matroska remux result. Only `mkv` sessions have this value.

The `endReason` field has one of these values:

| Value | Meaning |
|-------|---------|
| `client_disconnect` | The client closed the connection before the stream ended. |
| `upstream_ended` | The upstream stream ended without an error. |
| `upstream_error` | The upstream stream failed. |
| `recovery_expired` | Upstream recovery stopped after its retry limit. |
| `viewer_lagged` | The client fell behind the live buffer. |
| `stream_restarted` | The upstream session restarted with a new stream generation. |
| `buffer_error` | A read from the live buffer failed. |
| `remux_input_error` | The input of the Matroska remux failed. |
| `remux_exited` | The `FFmpeg` remux process stopped before its input ended. |
| `remux_output_error` | A read from the Matroska remux failed. |
| `start_failed` | The stream did not start. |

The `remux` object has these fields:

- `firstEnd`: the remux side that ended first, `input` or `output`.
- `exitCode` or `exitSignal`: the `FFmpeg` exit status.
- `inputError`: the redacted input error message.

The relay also writes each session to the service log at the info level. The `stream session started` and `stream session ended` events have the same fields.

A record never contains an output token, a credential, or an upstream URL.
