# Jellyfin

The gateway produces token-protected M3U and XMLTV endpoints that Jellyfin reads as an M3U tuner.

## Output URLs

The output endpoints use the token from `IPTV_OUTPUT_TOKEN`. Replace `{token}` with your output token.

The environment output profile initially includes all enabled channels. Its tuner count comes from `IPTV_TUNER_COUNT`.

| Endpoint | URL |
|----------|-----|
| M3U playlist | `http://localhost:8080/out/{token}/playlist.m3u` |
| XMLTV guide | `http://localhost:8080/out/{token}/xmltv.xml` |
| Stream | `http://localhost:8080/out/{token}/stream/{channel_path}` |

Set `IPTV_PUBLIC_BASE_URL` to the public address of your gateway. The M3U playlist uses this value for stream URLs.

## Save the Jellyfin configuration

Save the Jellyfin configuration with the API:

```bash
curl -X PUT http://localhost:8080/api/v1/jellyfin \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"url": "http://jellyfin:8096"}'
```

## Add the tuner to Jellyfin

1. Open the Jellyfin dashboard.
2. Go to **Live TV** and select **Tuners**.
3. Add a new M3U tuner.
4. Enter the M3U playlist URL.
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
