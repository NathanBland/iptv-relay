#!/usr/bin/env python3
"""Run the opt-in real-provider acceptance path through a disposable Jellyfin."""

from __future__ import annotations

import argparse
import base64
import gzip
import io
import json
import os
import re
import secrets
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
TERMINAL = {"succeeded", "failed", "cancelled", "dead"}
LIVE_KEYS = {"IPTV_TEST_XMLTV_URL", "XMLTV_URL", "LIVE_ACCEPTANCE_PROVIDER_CAP", "IPTV_TEST_PROVIDER_MAX_CONNECTIONS"}
XTREAM_KEYS = {"URL", "USER", "PWD"}


def interrupt_runner(signum: int, _frame: Any) -> None:
    raise KeyboardInterrupt(f"runner interrupted by signal {signum}")


def parse_env(path: Path, allowed: set[str]) -> dict[str, str]:
    values: dict[str, str] = {}
    for line_number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)=(.*)", line)
        if not match:
            raise RuntimeError(f"invalid environment record on line {line_number}")
        key, value = match.groups()
        if key in allowed:
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
                value = value[1:-1]
            values[key] = value
    return values


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def compose(env_file: Path, project: str, override: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    command = ["docker-compose", "--project-name", project, "-f", str(ROOT / "docker-compose.yml"), "-f", str(override), "--env-file", str(env_file), *args]
    result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=False)
    if check and result.returncode:
        detail = (result.stderr or result.stdout).strip().splitlines()
        raise RuntimeError(f"Compose failed: {' '.join(args)}: {' '.join(detail[-8:])}")
    return result


def remaining_volumes(project: str) -> str:
    result = subprocess.run(
        ["docker", "volume", "ls", "--quiet", "--filter", f"label=com.docker.compose.project={project}"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    return result.stdout.strip() if result.returncode == 0 else "unknown"


def request(url: str, method: str = "GET", body: Any = None, token: str | None = None, timeout: float = 30) -> Any:
    headers = {"Accept": "application/json"}
    if token:
        headers["X-Emby-Token"] = token
        headers["Authorization"] = f"Bearer {token}"
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            payload = response.read()
            if not payload:
                return None
            content_type = response.headers.get("content-type", "")
            return json.loads(payload) if "json" in content_type or payload[:1] in (b"{", b"[") else payload
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, json.JSONDecodeError) as error:
        if isinstance(error, urllib.error.HTTPError):
            error.read(256)
            detail = f" status={error.code}"
        else:
            detail = ""
        raise RuntimeError(f"request failed: {method} {url.split('?', 1)[0]}{detail} ({type(error).__name__})") from None


def wait_http(url: str, timeout: float = 300) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            request(url, timeout=5)
            return
        except RuntimeError:
            time.sleep(2)
    raise RuntimeError("service did not become ready before the deadline")


def xtream_endpoint(base: str, user: str, password: str, name: str) -> str:
    root = base.rstrip("/")
    for suffix in ("/player_api.php", "/xmltv.php"):
        if root.endswith(suffix):
            root = root[: -len(suffix)]
    query = urllib.parse.urlencode({"username": user, "password": password})
    return f"{root}/{name}?{query}"


def page_items(payload: Any) -> list[dict[str, Any]]:
    if isinstance(payload, list):
        return [item for item in payload if isinstance(item, dict)]
    if isinstance(payload, dict):
        for key in ("items", "Items", "channels", "Channels", "programs", "Programs"):
            if isinstance(payload.get(key), list):
                return [item for item in payload[key] if isinstance(item, dict)]
    return []


def extract_provider_ids(payload: Any) -> set[str]:
    """Return provider identities from Xtream streams without using names."""
    return {
        str(item.get("epg_channel_id") or item.get("stream_id") or "").strip()
        for item in page_items(payload)
        if str(item.get("epg_channel_id") or item.get("stream_id") or "").strip()
    }


def provider_streams_by_number(payload: Any) -> dict[str, str]:
    """Return the first Xtream stream identity for each provider channel number."""
    streams: dict[str, str] = {}
    for item in page_items(payload):
        number = str(item.get("num") or item.get("channel_number") or "").strip()
        stream_id = str(item.get("stream_id") or "").strip()
        if number and stream_id:
            streams.setdefault(number, stream_id)
    return streams


def provider_stream_has_ts(base: str, user: str, password: str, stream_id: str) -> bool:
    """Check a provider stream without logging its URL or credentials."""
    root = base.rstrip("/")
    for suffix in ("/player_api.php", "/xmltv.php"):
        if root.endswith(suffix):
            root = root[: -len(suffix)]
    url = f"{root}/live/{urllib.parse.quote(user, safe='')}/{urllib.parse.quote(password, safe='')}/{urllib.parse.quote(stream_id, safe='')}.ts"
    request = urllib.request.Request(url, headers={"Accept": "video/mp2t", "User-Agent": "iptv-live-acceptance"})
    try:
        with urllib.request.urlopen(request, timeout=12) as response:
            payload = response.read(188 * 4)
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, OSError):
        return False
    return len(payload) >= 188 and len(payload) % 188 == 0 and payload[0] == 0x47


def xmltv_channel_ids(url: str) -> set[str]:
    request = urllib.request.Request(url, headers={"Accept-Encoding": "gzip"})
    try:
        with urllib.request.urlopen(request, timeout=90) as response:
            payload = response.read()
            if response.headers.get("Content-Encoding", "").lower() == "gzip":
                payload = gzip.decompress(payload)
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, OSError):
        raise RuntimeError("XMLTV provider request failed") from None
    try:
        return {
            str(node.attrib.get("id", "")).strip()
            for node in ET.parse(io.BytesIO(payload)).getroot().findall("channel")
            if str(node.attrib.get("id", "")).strip()
        }
    except (ET.ParseError, OSError):
        raise RuntimeError("XMLTV provider payload is invalid") from None


def storage_bytes(path: Path) -> int:
    total = 0
    for item in path.rglob("*"):
        if item.is_file():
            try:
                total += item.stat().st_size
            except OSError:
                pass
    return total


def sessions_state(core: str, token: str) -> tuple[list[dict[str, Any]], int, int]:
    sessions = page_items(request(f"{core}/api/v1/sessions", token=token))
    pools: dict[str, tuple[int, int]] = {}
    for item in sessions:
        pool_id = str(item.get("providerPoolId") or item.get("sourceId") or "")
        active = int(item.get("providerActiveSessions", 0) or 0)
        available = int(item.get("providerAvailableSlots", 0) or 0)
        previous = pools.get(pool_id, (0, 0))
        pools[pool_id] = (max(previous[0], active), max(previous[1], available))
    active = sum(value[0] for value in pools.values())
    available = sum(value[1] for value in pools.values())
    return sessions, active, available


def wait_sessions(core: str, token: str, expected: int, timeout: float = 90) -> tuple[list[dict[str, Any]], int, int]:
    deadline = time.monotonic() + timeout
    last: tuple[list[dict[str, Any]], int, int] = ([], 0, 0)
    while time.monotonic() < deadline:
        last = sessions_state(core, token)
        if last[1] >= expected:
            return last
        time.sleep(1)
    raise RuntimeError(f"gateway did not report {expected} active provider sessions")


def wait_no_sessions(core: str, token: str, timeout: float = 90) -> tuple[list[dict[str, Any]], int, int]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        state = sessions_state(core, token)
        if not state[0] and state[1] == 0:
            return state
        time.sleep(1)
    raise RuntimeError("gateway retained live sessions after Jellyfin streams stopped")


def post_first(base: str, paths: list[str], body: Any, token: str) -> Any:
    last: BaseException | None = None
    for path in paths:
        try:
            return request(base + path, "POST", body, token)
        except RuntimeError as error:
            last = error
    raise RuntimeError(f"Jellyfin did not accept the requested refresh ({type(last).__name__})")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live-env", default=".env.live")
    parser.add_argument("--xtream-env", default=".env.xtreme")
    parser.add_argument("--soak-seconds", type=int, default=int(os.environ.get("JELLYFIN_ACCEPTANCE_SECONDS", "60")))
    parser.add_argument("--refresh-cycles", type=int, default=int(os.environ.get("JELLYFIN_ACCEPTANCE_REFRESH_CYCLES", "3")))
    parser.add_argument("--report")
    parser.add_argument("--keep", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        assert xtream_endpoint("https://provider.example/base", "user", "password", "player_api.php").endswith("/player_api.php?username=user&password=password")
        assert extract_provider_ids([{"epg_channel_id": "one"}, {"stream_id": 2}]) == {"one", "2"}
        assert provider_streams_by_number([{"num": 7, "stream_id": 42}]) == {"7": "42"}
        print("live Jellyfin acceptance self-test passed")
        return 0
    live = parse_env(ROOT / args.live_env, LIVE_KEYS)
    xtream = parse_env(ROOT / args.xtream_env, XTREAM_KEYS)
    if not all(xtream.get(key) for key in XTREAM_KEYS):
        raise RuntimeError(".env.xtreme must define URL, USER, and PWD")
    xmltv = (live.get("XMLTV_URL") or live.get("IPTV_TEST_XMLTV_URL") or xtream_endpoint(xtream["URL"], xtream["USER"], xtream["PWD"], "xmltv.php")).strip()
    if not xmltv:
        raise RuntimeError("no XMLTV URL is configured")
    if args.soak_seconds < 1:
        raise RuntimeError("soak duration must be positive")
    if args.refresh_cycles < 1:
        raise RuntimeError("refresh cycle count must be positive")
    capacity_text = live.get("LIVE_ACCEPTANCE_PROVIDER_CAP") or live.get("IPTV_TEST_PROVIDER_MAX_CONNECTIONS") or "2"
    try:
        provider_capacity = int(capacity_text)
    except ValueError:
        raise RuntimeError("provider capacity must be an integer") from None
    if provider_capacity < 2:
        raise RuntimeError("provider capacity must allow two concurrent streams")

    signal.signal(signal.SIGINT, interrupt_runner)
    signal.signal(signal.SIGTERM, interrupt_runner)

    project = f"iptv-jellyfin-live-{secrets.token_hex(4)}"
    root = Path(tempfile.mkdtemp(prefix="iptv-jellyfin-live-"))
    env_file = root / "compose.env"
    override = root / "compose.override.yml"
    core_port, gateway_port, postgres_port, jellyfin_port = (free_port() for _ in range(4))
    bootstrap, output, db_password, admin_password = (secrets.token_urlsafe(32) for _ in range(4))
    master_key = base64.b64encode(secrets.token_bytes(32)).decode()
    env = {
        "POSTGRES_PASSWORD": db_password, "IPTV_OUTPUT_TOKEN": output,
        "IPTV_ADMIN_BOOTSTRAP_TOKEN": bootstrap, "IPTV_ADMIN_PASSWORD": admin_password,
        "IPTV_MASTER_KEY": master_key, "IPTV_PUBLIC_BASE_URL": "http://gateway:8080",
        "IPTV_GATEWAY_PORT": str(gateway_port), "IPTV_POSTGRES_PORT": str(postgres_port),
        "IPTV_WORKER_COUNT": "1", "JELLYFIN_IMAGE": os.environ.get("JELLYFIN_IMAGE", "jellyfin/jellyfin:10.10.7"),
    }
    env_file.write_text("\n".join(f"{key}={value}" for key, value in env.items()) + "\n", encoding="utf-8")
    override.write_text("""services:
  core:
    ports:
      - 127.0.0.1:%s:8081
    environment:
      IPTV_DEV_MODE: "true"
      IPTV_DEV_AUTH_DISABLED: "true"
  gateway:
    environment:
      IPTV_PUBLIC_BASE_URL: http://gateway:8080
  jellyfin:
    image: ${JELLYFIN_IMAGE}
    ports:
      - 127.0.0.1:%s:8096
    volumes:
      - jellyfin-config:/config
      - jellyfin-cache:/cache
      - jellyfin-transcode:/config/transcodes
    depends_on:
      gateway:
        condition: service_started
    restart: "no"
volumes:
  jellyfin-config:
  jellyfin-cache:
  jellyfin-transcode:
""" % (core_port, jellyfin_port), encoding="utf-8")
    report: dict[str, Any] = {
        "soakSeconds": args.soak_seconds,
        "jellyfinImage": env["JELLYFIN_IMAGE"],
        "storage": {"temporaryBytesPeak": 0, "temporaryBytesFinal": 0},
    }
    failure: BaseException | None = None
    cleanup_ok = True
    try:
        compose(env_file, project, override, "up", "--build", "-d", "--wait", "postgres", "core", "web", "gateway", "worker", "jellyfin")
        core = f"http://127.0.0.1:{core_port}"
        jf = f"http://127.0.0.1:{jellyfin_port}"
        wait_http(f"{core}/health/ready")
        wait_http(f"{jf}/health")
        report["storage"]["temporaryBytesPeak"] = storage_bytes(root)
        stream_payload = request(xtream_endpoint(xtream["URL"], xtream["USER"], xtream["PWD"], "player_api.php") + "&action=get_live_streams")
        live_provider_ids = extract_provider_ids(stream_payload)
        if not live_provider_ids:
            raise RuntimeError("Xtream returned no usable provider identities")
        xmltv_ids = xmltv_channel_ids(xmltv)
        shared_source_ids = live_provider_ids & xmltv_ids
        if not shared_source_ids:
            raise RuntimeError("Xtream and XMLTV produced no shared provider identities")
        source = request(f"{core}/api/v1/sources", "POST", {"name": "live-jellyfin-xtream", "kind": "Xtream", "endpoint": xtream_endpoint(xtream["URL"], xtream["USER"], xtream["PWD"], "player_api.php"), "timezone": "UTC"}, bootstrap)
        request(f"{core}/api/v1/sources/{source['id']}", "PATCH", {"maxConnections": provider_capacity}, bootstrap)
        guide = request(f"{core}/api/v1/sources", "POST", {"name": "live-jellyfin-xmltv", "kind": "XMLTV", "endpoint": xmltv, "timezone": "UTC"}, bootstrap)
        source_cycles: list[dict[str, Any]] = []
        for cycle in range(args.refresh_cycles):
            for item in (source, guide):
                try:
                    request(f"{core}/api/v1/sources/{item['id']}/sync", "POST", token=bootstrap)
                except RuntimeError as error:
                    if "409" not in str(error):
                        raise
                deadline = time.monotonic() + 1800
                while time.monotonic() < deadline:
                    state = request(f"{core}/api/v1/sources/{item['id']}/sync-status", token=bootstrap)
                    if str(state.get("status", "")).lower() in TERMINAL:
                        if str(state.get("status", "")).lower() != "succeeded":
                            raise RuntimeError("source synchronization failed")
                        break
                    time.sleep(2)
                else:
                    raise RuntimeError("source synchronization exceeded deadline")
            mappings = page_items(request(f"{core}/api/v1/epg/mappings?limit=5000&offset=0", token=bootstrap))
            mapped_provider_ids = {
                str(item.get("canonicalKey") or item.get("epgXmltvId") or "").strip()
                for item in mappings
                if str(item.get("canonicalKey") or item.get("epgXmltvId") or "").strip()
            }
            shared_ids = shared_source_ids & mapped_provider_ids
            if not shared_ids:
                raise RuntimeError("gateway reconciliation produced no shared provider identities")
            source_cycles.append({"cycle": cycle + 1, "sharedProviderIds": len(shared_ids)})
            report["storage"]["temporaryBytesPeak"] = max(report["storage"]["temporaryBytesPeak"], storage_bytes(root))
        report["sourceRefreshCycles"] = source_cycles
        report["sharedProviderIds"] = len(shared_ids)
        report["providerCapacityConfigured"] = provider_capacity
        request(f"{jf}/Startup/Configuration", "POST", {"ServerName": "IPTV Acceptance", "UICulture": "en-US", "MetadataCountryCode": "US", "PreferredMetadataLanguage": "en"})
        request(f"{jf}/Startup/User")
        request(f"{jf}/Startup/User", "POST", {"Name": "acceptance", "Password": admin_password})
        request(f"{jf}/Startup/Complete", "POST", {})
        auth_header = {"X-Emby-Authorization": 'MediaBrowser Client="iptv-live-acceptance", Device="runner", DeviceId="runner", Version="1"'}
        # AuthenticateByName needs a special header, so call it directly.
        req = urllib.request.Request(f"{jf}/Users/AuthenticateByName", data=json.dumps({"Username": "acceptance", "Pw": admin_password}).encode(), headers={**auth_header, "Content-Type": "application/json"}, method="POST")
        with urllib.request.urlopen(req, timeout=30) as response:
            session = json.loads(response.read())
        jf_token = str(session.get("AccessToken", ""))
        user_id = str(session.get("User", {}).get("Id", ""))
        if not jf_token or not user_id:
            raise RuntimeError("Jellyfin authentication returned no session")
        tuner_url = "http://gateway:8080/out/%s/hdhr" % output
        guide_url = "http://gateway:8080/out/%s/xmltv.xml" % output
        tuner = request(f"{jf}/LiveTv/TunerHosts", "POST", {"Type": "hdhomerun", "TunerCount": 3, "DeviceId": secrets.token_hex(4).upper(), "Url": tuner_url}, jf_token)
        provider = request(f"{jf}/LiveTv/ListingProviders", "POST", {"Type": "XmlTv", "Path": guide_url, "Enabled": True}, jf_token)
        report["jellyfin"] = {"tunerConfigured": bool(tuner is not None), "guideConfigured": bool(provider is not None)}
        request(f"{jf}/LiveTv/Tuners/Discover?newDevicesOnly=false", token=jf_token)
        tasks = page_items(request(f"{jf}/ScheduledTasks", token=jf_token))
        guide_task = next((item for item in tasks if "refresh guide" in str(item.get("Name", "")).lower()), None)
        if guide_task and guide_task.get("Id"):
            request(f"{jf}/ScheduledTasks/Running/{guide_task['Id']}", "POST", token=jf_token)
        channels = []
        deadline = time.monotonic() + 900
        while time.monotonic() < deadline:
            channels = page_items(request(f"{jf}/LiveTv/Channels?UserId={user_id}&EnableImages=false", token=jf_token))
            if len(channels) >= 2:
                break
            time.sleep(3)
        if len(channels) < 2:
            raise RuntimeError("Jellyfin imported fewer than two channels")
        core_channels = page_items(request(f"{core}/api/v1/channels?limit=5000&offset=0", token=bootstrap))
        gateway_by_number = {
            str(item.get("number") or "").strip(): item
            for item in core_channels
            if str(item.get("number") or "").strip()
        }
        gateway_numbers = set(gateway_by_number)
        eligible_numbers = {number for number, item in gateway_by_number.items() if int(item.get("streams", 0) or 0) > 0}
        provider_streams = provider_streams_by_number(stream_payload)
        usable_numbers: set[str] = set()
        for item in channels:
            number = str(item.get("Number") or item.get("ChannelNumber") or "").strip()
            if number not in eligible_numbers or number in usable_numbers:
                continue
            stream_id = provider_streams.get(number)
            if stream_id and provider_stream_has_ts(xtream["URL"], xtream["USER"], xtream["PWD"], stream_id):
                usable_numbers.add(number)
            if len(usable_numbers) >= 2:
                break
        if len(usable_numbers) < 2:
            raise RuntimeError("Xtream did not provide two mapped channels with MPEG-TS data")
        selected = [item for item in channels if str(item.get("Number") or item.get("ChannelNumber") or "").strip() in usable_numbers][:2]
        if len(selected) < 2:
            raise RuntimeError("Jellyfin did not import two channels with gateway stream mappings")
        ids = [(str(item.get("Id", "")), str(item.get("Number", "")), str(item.get("ChannelNumber", ""))) for item in selected]
        if any(not item[0] for item in ids):
            raise RuntimeError("Jellyfin returned a channel without an ID")
        selected_numbers = {number or fallback for _, number, fallback in ids if number or fallback}
        if not selected_numbers or not selected_numbers & gateway_numbers:
            raise RuntimeError("Jellyfin channel numbers do not overlap gateway channels")
        mapping_items = list(mappings)
        mapping_total = int(request(f"{core}/api/v1/epg/mappings?limit=1&offset=0", token=bootstrap).get("total", len(mapping_items)) or len(mapping_items))
        for offset in range(5000, mapping_total, 5000):
            mapping_items.extend(page_items(request(f"{core}/api/v1/epg/mappings?limit=5000&offset={offset}", token=bootstrap)))
        mapping_by_channel = {
            str(item.get("channelId", "")): item
            for item in mapping_items
            if str(item.get("channelId", "")).strip()
        }
        provider_identity = []
        for item, (channel_id, number, fallback) in zip(selected, ids):
            provider_ids = item.get("ProviderIds") if isinstance(item.get("ProviderIds"), dict) else {}
            identity = next((str(value).strip() for value in provider_ids.values() if str(value).strip() in shared_ids), "")
            gateway_channel = gateway_by_number.get(number or fallback, {})
            mapping = mapping_by_channel.get(str(gateway_channel.get("id", "")), {})
            identity = identity or str(mapping.get("canonicalKey") or mapping.get("epgXmltvId") or "").strip()
            if identity:
                provider_identity.append(identity)
        if len(provider_identity) != 2:
            raise RuntimeError("Jellyfin channels did not resolve to provider identities")
        if not set(provider_identity).issubset(shared_ids):
            raise RuntimeError("Jellyfin channel mappings did not match reconciled provider identities")
        selected_gateway_ids = {
            str(gateway_by_number[number].get("id", ""))
            for _, number, fallback in ids
            if (number or fallback) in gateway_by_number
        }
        report["channels"] = [{"id": item[0], "number": item[1] or item[2], "providerIdentity": identity} for item, identity in zip(ids, provider_identity)]
        # A second provider refresh must preserve Jellyfin's identity and number.
        if guide_task and guide_task.get("Id"):
            request(f"{jf}/ScheduledTasks/Running/{guide_task['Id']}", "POST", token=jf_token)
            time.sleep(2)
            refreshed = page_items(request(f"{jf}/LiveTv/Channels?UserId={user_id}&EnableImages=false", token=jf_token))
            by_id = {str(item.get("Id", "")): str(item.get("Number", "") or item.get("ChannelNumber", "")) for item in refreshed}
            if any(by_id.get(channel_id) != (number or fallback) for channel_id, number, fallback in ids):
                raise RuntimeError("Jellyfin channel identity or number changed after refresh")
        # Open both streams through Jellyfin's Live TV API. Do not open the
        # gateway URL directly because that would skip Jellyfin's tuner path.
        streams: list[dict[str, Any]] = []
        for channel_id, _, _ in ids:
            opened = request(
                f"{jf}/Items/{urllib.parse.quote(channel_id, safe='')}/PlaybackInfo?UserId={urllib.parse.quote(user_id, safe='')}",
                "POST",
                {
                    "UserId": user_id,
                    "PlaySessionId": secrets.token_hex(8),
                    "AutoOpenLiveStream": True,
                    "EnableDirectPlay": True,
                    "EnableDirectStream": True,
                    "DeviceProfile": {
                        "Name": "IPTV acceptance",
                        "SupportedMediaTypes": "Audio,Video",
                        "MaxStreamingBitrate": 120000000,
                        "DirectPlayProfiles": [{"Container": "ts", "Type": "Video", "VideoCodec": "h264,mpeg2video", "AudioCodec": "aac,mp2"}],
                        "TranscodingProfiles": [{"Container": "ts", "Type": "Video", "Protocol": "http", "VideoCodec": "h264,mpeg2video", "AudioCodec": "aac,mp2"}],
                    },
                },
                jf_token,
                timeout=120,
            )
            if not isinstance(opened, dict):
                raise RuntimeError("Jellyfin did not return a live stream record")
            media_sources = opened.get("MediaSources") if isinstance(opened.get("MediaSources"), list) else []
            media_source = next((item for item in media_sources if isinstance(item, dict)), None)
            if media_source is None:
                raise RuntimeError("Jellyfin playback info did not include a live media source")
            stream_id = str(media_source.get("Id") or "")
            stream_path = str(media_source.get("Path") or "")
            match = re.search(r"/LiveStreamFiles/([^/]+)/stream", stream_path)
            stream_id = match.group(1) if match else stream_id
            if not stream_id:
                raise RuntimeError("Jellyfin did not return a live stream identity")
            streams.append({"streamId": stream_id, "liveStreamId": str(media_source.get("LiveStreamId") or "")})
        if len(streams) != 2:
            raise RuntimeError("Jellyfin did not open two live streams")
        started = time.monotonic()
        byte_counts = [0, 0]
        errors: list[BaseException] = []

        def consume(index: int, stream_id: str) -> None:
            try:
                target = f"{jf}/LiveTv/LiveStreamFiles/{urllib.parse.quote(stream_id, safe='')}/stream.ts"
                req = urllib.request.Request(target, headers={"X-Emby-Token": jf_token, "Accept": "video/mp2t"})
                with urllib.request.urlopen(req, timeout=args.soak_seconds + 30) as response:
                    byte_counts[index] = len(response.read(188 * 20))
                    time.sleep(args.soak_seconds)
            except BaseException as error:
                errors.append(error)

        readers = [threading.Thread(target=consume, args=(index, stream["streamId"]), daemon=True) for index, stream in enumerate(streams)]
        for reader in readers:
            reader.start()
        sessions, active, available = wait_sessions(core, bootstrap, expected=2)
        report["gatewayDuringStreams"] = {
            "sessionCount": len(sessions),
            "providerActiveSessions": active,
            "providerAvailableSlots": available,
            "distinctSessionIdentities": len({(item.get("providerPoolId"), item.get("sourceId"), item.get("configuredGeneration")) for item in sessions}),
        }
        session_channel_ids = {str(item.get("sourceId", "")) for item in sessions}
        if active != 2 or report["gatewayDuringStreams"]["distinctSessionIdentities"] < 2 or session_channel_ids != selected_gateway_ids:
            raise RuntimeError("gateway did not expose two distinct active provider sessions")
        for reader in readers:
            reader.join(args.soak_seconds + 60)
        if any(reader.is_alive() for reader in readers):
            raise RuntimeError("Jellyfin stream consumers exceeded their deadline")
        if errors:
            raise RuntimeError("Jellyfin live stream request failed") from errors[0]
        report["streams"] = {"count": 2, "bytes": byte_counts, "durationSeconds": round(time.monotonic() - started, 2)}
        if any(count == 0 or count % 188 for count in byte_counts):
            raise RuntimeError("Jellyfin stream did not return MPEG-TS bytes")
        for stream in streams:
            live_stream_id = stream.get("liveStreamId")
            if live_stream_id:
                request(
                    f"{jf}/LiveStreams/Close?liveStreamId={urllib.parse.quote(live_stream_id, safe='')}",
                    "POST",
                    token=jf_token,
                )
        _, active, available = wait_no_sessions(core, bootstrap)
        report["gatewayAfterStreams"] = {"providerActiveSessions": active, "providerAvailableSlots": available}
    except BaseException as error:
        failure = error
    finally:
        if not args.keep:
            down = compose(env_file, project, override, "down", "--volumes", "--remove-orphans", check=False)
            remaining = compose(env_file, project, override, "ps", "-aq", check=False)
            report["storage"]["temporaryBytesFinal"] = storage_bytes(root)
            volumes = remaining_volumes(project)
            report["cleanup"] = {"composeDown": down.returncode == 0, "remainingContainers": bool(remaining.stdout.strip()), "remainingVolumes": bool(volumes and volumes != "unknown"), "temporaryFilesRemoved": False}
            env_file.unlink(missing_ok=True)
            override.unlink(missing_ok=True)
            try:
                root.rmdir()
                report["cleanup"]["temporaryFilesRemoved"] = True
            except OSError:
                report["cleanup"]["temporaryFilesRemoved"] = False
            cleanup_ok = down.returncode == 0 and not remaining.stdout.strip() and not report["cleanup"]["remainingVolumes"] and report["cleanup"]["temporaryFilesRemoved"]
    verified = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True, capture_output=True, check=True).stdout.strip()
    report["verifiedCommit"] = verified
    if args.report:
        Path(args.report).write_text(json.dumps(report, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(report, sort_keys=True), flush=True)
    if failure:
        print(f"live Jellyfin acceptance failed: {failure}", file=sys.stderr)
        return 1
    if not cleanup_ok or report.get("cleanup", {}).get("remainingContainers") or report.get("cleanup", {}).get("remainingVolumes") or not report.get("cleanup", {}).get("temporaryFilesRemoved"):
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
