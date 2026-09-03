# System API

The system API exposes runtime diagnostics and redacted support data. The system and support endpoints require admin authentication.

## Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/system` | Return system counts and runtime versions. |
| GET | `/api/v1/support/bundle` | Return a redacted support bundle. Requires admin. |
| GET | `/api/v1/support/logs` | Return recent redacted support log entries. Requires admin. |

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
