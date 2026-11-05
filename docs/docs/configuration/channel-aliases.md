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

## Name precedence

The relay chooses the output name of a channel in this order:

1. The name override that an operator sets on the channel.
2. The canonical name of a matching alias.
3. The stable event name of a dynamic event channel.
4. The provider name.

The playlist, the guide, and the tuner lineup use the output name.

## Rename a channel

To change the output name of one channel, set a name override. A name override has precedence over an alias.

1. Open the **Channels** page.
2. Select the pencil icon next to the channel name.
3. Enter the new name.
4. Select **Save**.

The **Channels** page shows the output name first. It shows the provider name on a second line when the two names are different.

To remove a name override, select **Reset**. The channel then uses the alias, the stable event name, or the provider name.

Use the API to set a name override:

```bash
curl -X PATCH http://localhost:8080/api/v1/channels/{channel_id}/name \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"nameOverride": "Local News"}'
```

A name override has a maximum of 200 characters. A provider refresh does not change a name override.

## Channel aliases page

The `/channel-aliases` page provides:

- A name resolver that shows the canonical name for a given input.
- A form to add new aliases.
- A list of all aliases with country and category badges.
- Delete buttons for each alias.
