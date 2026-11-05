# Output API

The output endpoints deliver M3U, XMLTV, and HDHomeRun-compatible data to clients. All output endpoints use a token in the path.

Replace `{token}` with the value of `IPTV_OUTPUT_TOKEN`.

At startup, the core stores the token hash in the environment output profile.

The initial profile includes all enabled channels. Output requests use the channel selection and tuner count from this profile.

After a token rotation, the previous token stays valid for the configured overlap window. The default overlap is five minutes.

The current UI does not configure output-profile channel selections.

## Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/out/{token}/playlist.m3u` | Return the M3U playlist. |
| GET | `/out/{token}/xmltv.xml` | Return the XMLTV guide. |
| GET | `/out/{token}/stream/{*channel_path}` | Stream a channel as MPEG-TS. |
| GET | `/out/{token}/hdhr/discover.json` | Return the HDHomeRun discovery document. |
| GET | `/out/{token}/hdhr/lineup.json` | Return the HDHomeRun lineup. |
| GET | `/out/{token}/hdhr/lineup_status.json` | Return the HDHomeRun lineup status. |
| GET | `/out/{token}/hdhr/device.xml` | Return the HDHomeRun device XML. |

## M3U playlist

```bash
curl http://localhost:8080/out/{token}/playlist.m3u
```

The playlist includes selected channels where `enabled = true`. Stream URLs use `IPTV_PUBLIC_BASE_URL` as the base.

## XMLTV guide

```bash
curl http://localhost:8080/out/{token}/xmltv.xml
```

The guide includes programmes for selected and enabled channels only.
The output writes normalized UTC times with a `+0000` offset.
The output includes imported programmes and active generated event or filler programmes.

## Stream a channel

```bash
curl http://localhost:8080/out/{token}/stream/{channel_path}
```

The endpoint delivers the raw MPEG-TS stream. The media plane shares one upstream provider session across multiple viewers.

### Stream slates

The endpoint does not send an HTTP error for a provider problem. When live media is not available, the endpoint sends a slate. A slate is a short video with a message:

| Condition | Slate message |
|---|---|
| The live buffer fills | Stream starting in N seconds |
| All provider connections are in use | All tuners in use, stay tuned for one to open up |
| The provider or the input adapter fails | Having trouble reaching this channel, retrying |

The relay retries the provider while the slate plays. Each retry opens at most one provider connection, and the relay obeys the connection limit of the provider pool.

- When the pool is full, the relay retries each 500 ms.
- After a provider error, the relay waits 1 s, then 2 s, then 4 s, up to 30 s.

When live media is ready, the relay splices it into the same response. The slate uses the last measured resolution, frame rate, and codecs of the channel. When no measured data exists, the slate uses 1280x720 H.264 at 30 fps with 48 kHz stereo AAC.

The endpoint sends an HTTP error only for a channel without a source or for a configuration error.

## HDHomeRun emulation

The HDHomeRun endpoints emulate an HDHomeRun device. Clients that require HDHomeRun discovery use these endpoints.

```bash
curl http://localhost:8080/out/{token}/hdhr/discover.json
```

```bash
curl http://localhost:8080/out/{token}/hdhr/lineup.json
```

## Jellyfin setup

The gateway does not accept a `PUT /api/v1/jellyfin` request. Use the Jellyfin setup endpoints to read the published URLs and to rotate the output token.

These endpoints require an authenticated session or an operator API token with the `output` or `admin` scope. Session mutations also require CSRF protection.

Read the published setup URLs:

```bash
curl http://localhost:8080/api/v1/jellyfin/setup \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN"
```

Rotate the output token:

```bash
curl -X POST http://localhost:8080/api/v1/jellyfin/setup/rotate \
  -H "Authorization: Bearer $IPTV_ADMIN_BOOTSTRAP_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"overlapSeconds": 300}'
```

See the [Jellyfin configuration guide](../configuration/jellyfin.md) for the full setup procedure.

## Security

The output token protects all output endpoints. Use a random 256-bit value for `IPTV_OUTPUT_TOKEN`. Do not expose the token in logs.

The database stores only the token hash. Output URLs contain the plaintext token in the path.
