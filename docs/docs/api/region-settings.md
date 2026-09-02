# Region Settings API

The region settings API controls the timezone and the enabled region prefixes that filter the channel catalog. All endpoints require admin authentication.

The gateway groups channels by region prefix. A region prefix is the country or language tag at the start of a group name. The suggested prefixes come from the configured timezone.

## Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/region-settings` | Return the current region settings and available prefixes. |
| PUT | `/api/v1/region-settings` | Update the timezone and enabled prefixes. |
| POST | `/api/v1/region-settings/apply` | Apply the enabled prefixes to the channel catalog. |

## Read the region settings

```bash
curl http://localhost:8080/api/v1/region-settings \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns a `RegionSettingsResponse` object. The object includes `settings` and `prefixes`.

The `settings` field is a `RegionSettingsDto` object. The object includes `timezone`, `enabledPrefixes`, `suggestedPrefixes`, and `autoDetected`.

The `prefixes` field is an array of `RegionPrefixResponse` objects. Each entry includes `prefix`, `groupCount`, `channelCount`, and `suggested`.

## Update the region settings

```bash
curl -X PUT http://localhost:8080/api/v1/region-settings \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "timezone": "America/Denver",
    "enabledPrefixes": ["US", "CA"]
  }'
```

The request body is an `UpdateRegionSettingsRequest` object. The object includes `timezone` and `enabledPrefixes`.

Use a valid IANA timezone name for the `timezone` value. The response returns the updated `RegionSettingsDto` object.

## Apply the region filter

```bash
curl -X POST http://localhost:8080/api/v1/region-settings/apply \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"enabledPrefixes": ["US", "CA"]}'
```

The request body is an `ApplyRegionFilterRequest` object with an `enabledPrefixes` array.

The endpoint enables the channels that match the listed prefixes and disables the channels that do not match. The response returns a `RegionFilterResponse` object with `enabled` and `disabled` counts.
