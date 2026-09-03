# Jellyfin

The gateway produces token-protected M3U and XMLTV endpoints. Jellyfin reads these endpoints as an M3U tuner.

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

## Add the tuner to Jellyfin

1. Open the Jellyfin dashboard.
2. Go to **Live TV** and select **Tuners**.
3. Add a new M3U tuner.
4. Enter the M3U playlist URL from the setup response.
5. Add the XMLTV guide URL in the guide settings.
6. Save the tuner.

Jellyfin reads the playlist and guide from the token-protected endpoints.

## HDHomeRun emulation

The gateway emulates an HDHomeRun device for clients that require it.

| Endpoint | URL |
|----------|-----|
| Discovery | `http://localhost:8080/out/{token}/hdhr/discover.json` |
| Lineup | `http://localhost:8080/out/{token}/hdhr/lineup.json` |
| Lineup status | `http://localhost:8080/out/{token}/hdhr/lineup_status.json` |
| Device XML | `http://localhost:8080/out/{token}/hdhr/device.xml` |

## Jellyfin setup page

The Jellyfin setup page shows the output URLs and the HDHomeRun discovery URL. Copy the URLs from the page to avoid manual entry.
