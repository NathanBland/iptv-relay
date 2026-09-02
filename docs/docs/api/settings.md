# Settings API

The settings API exposes the operator-facing configuration catalog and the override store. All endpoints require admin authentication.

The catalog lists the settings that the backend owns. Each setting has a stable key, a value kind, a default value, and an apply requirement.

Overrides change a setting value at one of three scopes: global, provider, or channel group. The effective settings endpoint resolves the inherited value for a given provider and group.

## Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/settings/schema` | Return the setting catalog. |
| GET | `/api/v1/settings/effective` | Return the effective settings for a provider and group. |
| GET | `/api/v1/settings/overrides` | Return all current overrides grouped by scope. |
| GET | `/api/v1/settings/overrides/global` | Return the global overrides and revision. |
| PUT | `/api/v1/settings/overrides/global` | Replace the global overrides. |
| GET | `/api/v1/settings/overrides/global/revisions` | Return the global revision history. |
| POST | `/api/v1/settings/overrides/global/rollback` | Roll back the global overrides to a revision. |
| GET | `/api/v1/settings/overrides/providers/{providerId}` | Return the provider overrides and revision. |
| PUT | `/api/v1/settings/overrides/providers/{providerId}` | Replace the provider overrides. |
| GET | `/api/v1/settings/overrides/providers/{providerId}/revisions` | Return the provider revision history. |
| POST | `/api/v1/settings/overrides/providers/{providerId}/rollback` | Roll back the provider overrides to a revision. |
| GET | `/api/v1/settings/overrides/groups/{groupId}` | Return the group overrides and revision. |
| PUT | `/api/v1/settings/overrides/groups/{groupId}` | Replace the group overrides. |
| GET | `/api/v1/settings/overrides/groups/{groupId}/revisions` | Return the group revision history. |
| POST | `/api/v1/settings/overrides/groups/{groupId}/rollback` | Roll back the group overrides to a revision. |

## Read the setting catalog

```bash
curl http://localhost:8080/api/v1/settings/schema \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns an array of `SettingDefinition` objects. Each definition includes `key`, `label`, `description`, `valueKind`, `defaultValue`, `risk`, and `applyRequirement`.

The `valueKind` value is `boolean`, `integer`, `string`, or `choice`. The `applyRequirement` value is `immediate`, `restart`, or `reimport`.

## Read the effective settings

```bash
curl "http://localhost:8080/api/v1/settings/effective?providerId=SOURCE_ID&groupId=GROUP_ID" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The `providerId` and `groupId` query parameters are optional. Omit a parameter when you do not target that scope.

The response returns an `EffectiveSettingsResponse` object. The object includes `providerId`, `groupId`, `settings`, `applyRequirements`, and `etag`.

Each `EffectiveSetting` entry includes the `definition`, the resolved `value`, and the `inheritedFrom` source. The `inheritedFrom` value is `system_default`, `global`, `provider`, or `channel_group`.

## Read all overrides

```bash
curl http://localhost:8080/api/v1/settings/overrides \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns an `OperatorOverridesResponse` object. The object includes `global`, `providers`, and `groups` maps. Each map contains the setting key and the override value.

## Read a scope

```bash
curl http://localhost:8080/api/v1/settings/overrides/global \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Replace `global` with `providers/{providerId}` or `groups/{groupId}` to read a different scope.

The response returns an `OperatorScopeResponse` object. The object includes `scope`, `scopeId`, `overrides`, and `revision`. The response sets the `ETag` header to the revision value.

## Replace a scope

```bash
curl -X PUT http://localhost:8080/api/v1/settings/overrides/global \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -H 'If-Match: "12"' \
  -d '{"overrides": {"ingest.max_download_bytes": 1073741824}}'
```

Send the `If-Match` header with the `ETag` value from the previous scope response. The header prevents concurrent overwrites.

The request body is an `ReplaceOperatorScopeRequest` object with an `overrides` map. The map replaces the full override set for the scope.

A `412` response indicates a revision conflict. Refresh the scope and retry the request. A `422` response indicates an invalid setting value.

## Read the revision history

```bash
curl http://localhost:8080/api/v1/settings/overrides/global/revisions \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns an array of `OperatorRevisionResponse` objects. Each entry includes `revision`, `actor`, `createdAt`, `beforeValue`, and `afterValue`. The list orders the entries from newest to oldest.

## Roll back a scope

```bash
curl -X POST http://localhost:8080/api/v1/settings/overrides/global/rollback \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"revision": 10}'
```

The request body is a `RollbackOperatorScopeRequest` object with a `revision` number. The endpoint restores the scope to the value at that revision.

A `404` response indicates that the revision does not exist for the scope.
