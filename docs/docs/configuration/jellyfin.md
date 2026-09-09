# Jellyfin

The gateway provides M3U, XMLTV, and HDHomeRun output endpoints for Jellyfin.

## Output URLs

The output endpoints use the token from `IPTV_OUTPUT_TOKEN`. Replace `{token}` with your output token.

The environment output profile initially includes all enabled channels. Its tuner count comes from `IPTV_TUNER_COUNT`.

| Endpoint | URL |
|----------|-----|
| M3U playlist | `http://localhost:8080/out/{token}/playlist.m3u` |
| XMLTV guide | `http://localhost:8080/out/{token}/xmltv.xml` |
| Stream | `http://localhost:8080/out/{token}/stream/{channel_path}` |

Set `IPTV_PUBLIC_BASE_URL` to the public address of your gateway. The M3U playlist uses this value for stream URLs.

## Read the Jellyfin setup

Read the published setup URLs with the API:

```bash
curl http://localhost:8080/api/v1/jellyfin/setup \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

The response returns a `JellyfinSetup` object. The `status` field is `available` when the URLs are present. The `status` field is `regeneration-required` after a token rotation.

The response includes `playlistUrl`, `xmltvUrl`, and `hdhrDeviceUrl`. The response omits these fields when regeneration is required.

The response sets `Cache-Control: no-store`. The token stays embedded in each URL path. The response never returns the token as a separate field.

## Rotate the output token

Rotate the environment output token with the API:

```bash
curl -X POST http://localhost:8080/api/v1/jellyfin/setup/rotate \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"overlapSeconds": 300}'
```

The endpoint generates a cryptographically random token. The endpoint stores only the SHA-256 hash of the new token. The endpoint returns the plaintext token once in the response URLs.

Set `overlapSeconds` between `0` and `86400`. The default value is `300`. During the overlap window, both the old and new tokens accept output requests.

Use an authenticated session or an operator API token with the `output` or `admin` scope for Jellyfin setup requests. A session mutation requires its CSRF header.

After the rotation, the `GET /api/v1/jellyfin/setup` endpoint returns `regeneration-required`. The plaintext token is not retained. Call the rotate endpoint again to receive a new plaintext token.

## Configure an M3U tuner

Use this configuration when you want Jellyfin to read the gateway M3U playlist.

1. Open the Jellyfin dashboard.
2. Select **Live TV**.
3. Select **Tuner Devices**.
4. Add a tuner.
5. Set **Tuner Type** to **M3U Tuner**.
6. Set **File or URL** to `playlistUrl` from the setup response.
7. Leave **User agent** empty.
8. Set **Simultaneous stream limit** to the value of `IPTV_TUNER_COUNT`.
9. Keep **Auto-loop live streams** disabled.
10. Save the tuner.

Set **Auto-loop live streams** only when the provider stream requires it.
The M3U output includes channel aliases and `tvg-logo` values when a logo is available.

## Configure XMLTV guide data

Add XMLTV after you save an M3U tuner or an HDHomeRun tuner.

1. Select **Live TV**.
2. Select **TV Guide Data Providers**.
3. Add a guide provider.
4. Set the provider type to **XMLTV**.
5. Set the XMLTV URL to `xmltvUrl` from the setup response.
6. Save the guide provider.
7. Select the guide provider menu.
8. Select **Map Channels**.
9. Map each tuner channel to the matching XMLTV channel.

The XMLTV output uses the same channel IDs as the M3U output.
The XMLTV output includes aliases and channel icons when a logo is available.

## HDHomeRun emulation

The gateway emulates an HDHomeRun device for clients that require it.

| Endpoint | URL |
|----------|-----|
| Discovery | `http://localhost:8080/out/{token}/hdhr/discover.json` |
| Lineup | `http://localhost:8080/out/{token}/hdhr/lineup.json` |
| Lineup status | `http://localhost:8080/out/{token}/hdhr/lineup_status.json` |
| Device XML | `http://localhost:8080/out/{token}/hdhr/device.xml` |

## Configure an HDHomeRun tuner

Use this configuration instead of an M3U tuner when you want HDHomeRun integration.
Do not add the same gateway channels through both tuner types.

1. Open the Jellyfin dashboard.
2. Select **Live TV**.
3. Select **Tuner Devices**.
4. Add a tuner.
5. Set **Tuner Type** to **HDHomeRun**.
6. Set **Tuner IP Address** to the HDHomeRun base URL.
7. Use `hdhrDeviceUrl` without the final `/device.xml` path component.
8. Disable **Allow hardware transcoding**.
9. Disable **Restrict to channels marked as favorite**.
10. Save the tuner.

For example, change this setup URL:

```text
https://gateway.example/out/{token}/hdhr/device.xml
```

To this tuner address:

```text
https://gateway.example/out/{token}/hdhr
```

The HDHomeRun lineup includes canonical channel aliases and `ImageURL` values when a logo is available.
Add XMLTV guide data after you save the HDHomeRun tuner.

## Jellyfin setup page

The Jellyfin setup page shows the output URLs and the HDHomeRun discovery URL. Copy the URLs from the page to avoid manual entry.
