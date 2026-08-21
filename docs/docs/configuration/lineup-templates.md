# Lineup Templates

A lineup template defines a set of categories, channels, and aliases. The apply step matches canonical channels by normalized name or alias and enables the matched channels.

## Template structure

A lineup template contains:

- A name.
- One or more categories.
- One or more channels per category.
- Optional aliases per channel.

## Create a template with the API

```bash
curl -X POST http://localhost:8080/api/v1/lineup-templates \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Sports lineup",
    "categories": [
      {
        "name": "Sports",
        "channels": [
          {"name": "ESPN", "aliases": ["ESPN HD", "ESPN UHD"]},
          {"name": "NFL Network", "aliases": ["NFL"]}
        ]
      }
    ]
  }'
```

All lineup endpoints require admin authentication.

## List categories and channels

List categories for a template:

```bash
curl http://localhost:8080/api/v1/lineup-templates/{template_id}/categories \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

List channels for a template:

```bash
curl http://localhost:8080/api/v1/lineup-templates/{template_id}/channels \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## Apply a lineup

Apply a lineup template to the catalog:

```bash
curl -X POST http://localhost:8080/api/v1/lineup-templates/{template_id}/apply \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The apply step:

1. Matches canonical channels by normalized name or alias.
2. Enables matched channels.
3. Disables unmatched channels in the group.

The response returns the count of matched channels.

## Delete a template

```bash
curl -X DELETE http://localhost:8080/api/v1/lineup-templates/{template_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## Performance

The `apply_lineup` method uses a single batch query with `unnest` for all lineup channels. A category with N lineup channels issues one round trip instead of N.
