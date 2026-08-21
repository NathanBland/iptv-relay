# Channel Aliases

The channel alias system standardizes channel names against a curated alias table. The `0015_channel_aliases.sql` migration creates the `channel_aliases` table with 98 pre-loaded aliases.

## Alias fields

| Field | Description |
|-------|-------------|
| `canonicalName` | The standardized channel name. |
| `alias` | The alternate name that maps to the canonical name. |
| `country` | Country code for the alias. |
| `category` | Category label for the alias. |

## Pre-loaded coverage

The migration loads aliases for these categories:

- US networks: ABC, CBS, NBC, FOX, PBS.
- US sports: ESPN, ESPN2, FS1, FS2, NFL Network, NBA TV, MLB Network, NHL Network.
- US entertainment: TNT, TBS, AMC, FX, USA, Syfy, Bravo, E!, Comedy Central.
- US news: CNN, Fox News, MSNBC, HLN.
- US premium: HBO, Showtime, Cinemax, Starz, Encore.
- US documentary: Discovery, History, Nat Geo, Animal Planet, Science, TLC.
- US lifestyle: Food Network, HGTV, DIY, Cooking Channel, Travel Channel.
- UK channels: BBC, BBC One, BBC Two, ITV, Channel 4, Channel 5, Sky One, Sky Sports, Sky News.

## Resolve a name

Resolve a name to its canonical form:

```bash
curl "http://localhost:8080/api/v1/channel-aliases/resolve?name=ESPN%20HD" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The endpoint returns the canonical name for the given input.

## List aliases

```bash
curl "http://localhost:8080/api/v1/channel-aliases?country=US" \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The endpoint accepts an optional country filter with pagination.

## Create an alias

```bash
curl -X POST http://localhost:8080/api/v1/channel-aliases \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "canonicalName": "ESPN",
    "alias": "ESPN 4K",
    "country": "US",
    "category": "sports"
  }'
```

The endpoint rejects duplicate aliases.

## Delete an alias

```bash
curl -X DELETE http://localhost:8080/api/v1/channel-aliases/{alias_id} \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

## Channel aliases page

The `/channel-aliases` page provides:

- A name resolver that shows the canonical name for a given input.
- A form to add new aliases.
- A list of all aliases with country and category badges.
- Delete buttons for each alias.
