#!/usr/bin/env python3
"""Run the real-provider API acceptance gate with the Python standard library."""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import gzip
import json
import os
import re
import secrets
import signal
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import urllib.parse
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
COMPOSE_OVERRIDE: Path | None = None
ALLOWED_ENV = {
    "URL", "USER", "PWD", "XMLTV_URL",
    "IPTV_TEST_XMLTV_URL", "IPTV_TEST_PROVIDER_MAX_CONNECTIONS", "LIVE_ACCEPTANCE_PROVIDER_CAP",
}
TERMINAL = {"succeeded", "failed", "cancelled", "dead"}


def parse_env(path: Path) -> dict[str, str]:
    """Parse KEY=value records without evaluating shell syntax."""
    values: dict[str, str] = {}
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)=(.*)", line)
        if not match:
            raise ValueError(f"invalid .env.live record on line {number}")
        key, value = match.groups()
        if key in ALLOWED_ENV:
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
                value = value[1:-1]
            values[key] = value
    return values


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def secret(length: int = 48) -> str:
    return secrets.token_urlsafe(length)


def request_json(base: str, path: str, token: str, method: str = "GET", body: Any = None) -> Any:
    headers = {"Authorization": f"Bearer {token}", "Accept": "application/json"}
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode("utf-8")
    request = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            payload = response.read()
            return None if not payload else json.loads(payload.decode("utf-8"))
    except urllib.error.HTTPError as error:
        detail = error.read(512).decode("utf-8", errors="replace").replace("\n", " ")
        raise RuntimeError(f"gateway request failed: {method} {path} status={error.code} detail={detail}") from None
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as error:
        raise RuntimeError(f"gateway request failed: {method} {path} detail={type(error).__name__}") from None


def download_file(url: str, destination: Path) -> None:
    """Stream a provider document to disk and transparently decompress gzip."""
    request = urllib.request.Request(url, headers={"Accept-Encoding": "gzip"})
    try:
        with urllib.request.urlopen(request, timeout=45) as response, destination.open("wb") as output:
            source = gzip.GzipFile(fileobj=response) if response.headers.get("Content-Encoding", "").lower() == "gzip" else response
            with source:
                shutil.copyfileobj(source, output, length=1024 * 1024)
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, OSError):
        raise RuntimeError("provider download failed") from None


def xtream_endpoint(base: str, username: str, password: str, filename: str, action: str | None = None) -> str:
    """Build an Xtream endpoint without exposing credentials in diagnostics."""
    root = base.rstrip("/")
    if root.endswith("/player_api.php"):
        root = root[:-len("/player_api.php")]
    if root.endswith("/xmltv.php"):
        root = root[:-len("/xmltv.php")]
    query = {"username": username, "password": password}
    if action:
        query["action"] = action
    return f"{root}/{filename}?{urllib.parse.urlencode(query)}"


def xtream_live_streams(base: str, username: str, password: str) -> list[dict[str, Any]]:
    """Authenticate and fetch live streams from a standard Xtream Codes API."""
    endpoint = xtream_endpoint(base, username, password, "player_api.php")
    try:
        with urllib.request.urlopen(endpoint, timeout=45) as response:
            auth = json.loads(response.read().decode("utf-8"))
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, json.JSONDecodeError):
        raise RuntimeError("Xtream authentication request failed") from None
    user_info = auth.get("user_info", {}) if isinstance(auth, dict) else {}
    if not isinstance(user_info, dict) or str(user_info.get("auth", "0")) not in {"1", "true", "True"}:
        raise RuntimeError("Xtream authentication was rejected")
    streams_url = xtream_endpoint(base, username, password, "player_api.php", "get_live_streams")
    try:
        with urllib.request.urlopen(streams_url, timeout=90) as response:
            streams = json.loads(response.read().decode("utf-8"))
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, json.JSONDecodeError):
        raise RuntimeError("Xtream get_live_streams request failed") from None
    if not isinstance(streams, list) or not all(isinstance(item, dict) for item in streams):
        raise RuntimeError("Xtream get_live_streams returned an invalid payload")
    return streams


def output_playlist(base: str, token: str) -> bytes:
    request = urllib.request.Request(f"{base}/out/{token}/playlist.m3u", headers={"Accept": "audio/x-mpegurl"})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.read()
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError):
        raise RuntimeError("gateway output playlist request failed") from None


def m3u_ids(payload: bytes) -> set[str]:
    ids: set[str] = set()
    for line in payload.decode("utf-8-sig", errors="replace").splitlines():
        if line.startswith("#EXTINF:"):
            match = re.search(r'(?:tvg-id|tvgid)=["\']?([^"\' ,]+)', line, re.I)
            if match and match.group(1):
                ids.add(match.group(1))
    return ids


def m3u_ids_file(path: Path) -> set[str]:
    ids: set[str] = set()
    with path.open("r", encoding="utf-8-sig", errors="replace") as stream:
        for line in stream:
            if line.startswith("#EXTINF:"):
                match = re.search(r'(?:tvg-id|tvgid|tvg-chno|channel-number|channel_number|number)=["\']?([^"\' ,]+)', line, re.I)
                if match and match.group(1):
                    ids.add(match.group(1))
    return ids


def m3u_names_file(path: Path) -> set[str]:
    names: set[str] = set()
    with path.open("r", encoding="utf-8-sig", errors="replace") as stream:
        for line in stream:
            if line.startswith("#EXTINF:"):
                name = line.rsplit(",", 1)[-1].strip()
                if name:
                    names.add(name)
    return names


def xmltv_file(path: Path) -> tuple[set[str], dict[str, list[tuple[str, str, str]]], set[str]]:
    ids: set[str] = set()
    programmes: dict[str, list[tuple[str, str, str]]] = {}
    names: set[str] = set()
    try:
        for event, node in ET.iterparse(path, events=("end",)):
            if node.tag == "channel" and node.attrib.get("id"):
                ids.add(node.attrib["id"])
                names.update((item.text or "").strip() for item in node.findall("display-name") if (item.text or "").strip())
            elif node.tag == "programme":
                channel = node.attrib.get("channel", "")
                title = node.findtext("title") or ""
                if channel and title:
                    programmes.setdefault(channel, []).append((title, node.attrib.get("start", ""), node.attrib.get("stop", "")))
                node.clear()
    except ET.ParseError:
        raise RuntimeError("provider XMLTV payload is invalid") from None
    return ids, programmes, names


def page_items(body: Any) -> list[dict[str, Any]]:
    if isinstance(body, list):
        return body
    if isinstance(body, dict) and isinstance(body.get("items"), list):
        return body["items"]
    raise RuntimeError("gateway page response has no items")


def programme_key(value: tuple[str, str, str]) -> tuple[str, str, str]:
    """Normalize XMLTV timestamps so equivalent UTC offsets compare equal."""
    title, start, stop = value
    normalized: list[str] = []
    for timestamp in (start, stop):
        try:
            parsed = dt.datetime.strptime(timestamp, "%Y%m%d%H%M%S %z")
            normalized.append(str(int(parsed.timestamp())))
        except ValueError:
            normalized.append(timestamp)
    return title, normalized[0], normalized[1]


def all_pages(base: str, token: str, path: str) -> list[dict[str, Any]]:
    items: list[dict[str, Any]] = []
    offset = 0
    while True:
        body = request_json(base, f"{path}?limit=500&offset={offset}", token)
        page = page_items(body)
        items.extend(page)
        total = int(body.get("total", len(items))) if isinstance(body, dict) else len(items)
        if not page or len(items) >= total:
            return items
        offset += len(page)


def channel_key(item: dict[str, Any]) -> str:
    # canonicalKey is the stable provider identity. Keep tvgId as a compatibility
    # fallback for older local binaries while the API migration rolls out.
    value = item.get("canonicalKey") or item.get("tvgId")
    return str(value) if value else ""


def compose(env_file: Path, project: str, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    command = ["docker-compose", "--parallel", "1"]
    if COMPOSE_OVERRIDE is not None:
        command.extend(["-f", str(ROOT / "docker-compose.yml"), "-f", str(COMPOSE_OVERRIDE)])
    command.extend(["--project-name", project, "--env-file", str(env_file), *args])
    inherited = {key: value for key, value in os.environ.items() if not (key.startswith("IPTV_") or key in {"POSTGRES_PASSWORD", "COMPOSE_PROJECT_NAME", "COMPOSE_PARALLEL_LIMIT", "CARGO_BUILD_JOBS"})}
    inherited["COMPOSE_PARALLEL_LIMIT"] = "1"
    inherited["CARGO_BUILD_JOBS"] = "1"
    inherited["DOCKER_BUILDKIT"] = "1"
    result = subprocess.run(command, cwd=ROOT, env=inherited, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if check and result.returncode:
        detail = (result.stderr or result.stdout).strip().splitlines()
        tail = "\\n".join(detail[-12:])
        raise RuntimeError(f"Compose command failed ({' '.join(args)}): {tail}")
    return result


def storage_metrics(env_file: Path, project: str) -> dict[str, int]:
    query = "SELECT pg_database_size(current_database()), (SELECT temp_bytes FROM pg_stat_database WHERE datname=current_database()), (SELECT count(*) FROM source_snapshots WHERE status='staging'), (SELECT count(*) FROM provider_reconciliation_candidates), (SELECT count(*) FROM jobs WHERE status IN ('queued','running'));"
    result = compose(env_file, project, "exec", "-T", "postgres", "psql", "-U", "iptv", "-d", "iptv", "-At", "-c", query, check=False)
    fields = result.stdout.strip().split("|")
    if result.returncode or len(fields) != 5:
        raise RuntimeError("storage metrics unavailable")
    try:
        return {"databaseBytes": int(fields[0]), "tempBytes": int(fields[1]), "stagingSnapshots": int(fields[2]), "reconciliationCandidates": int(fields[3]), "pendingJobs": int(fields[4])}
    except ValueError:
        raise RuntimeError("storage metrics unavailable") from None


def wait_sync(base: str, token: str, source_id: str, deadline: float) -> dict[str, Any]:
    while time.monotonic() < deadline:
        status = request_json(base, f"/api/v1/sources/{source_id}/sync-status", token)
        state = str(status.get("status", "")).lower()
        if state in TERMINAL:
            if state != "succeeded":
                raise RuntimeError(f"source sync ended with {state}")
            return status
        time.sleep(1)
    raise RuntimeError("source sync exceeded its deadline")


def ensure_source(base: str, token: str, name: str, kind: str, endpoint: str) -> str:
    sources = page_items(request_json(base, "/api/v1/sources", token))
    existing = next((item for item in sources if item.get("name") == name), None)
    if existing:
        if str(existing.get("kind", "")).lower() != kind.lower():
            raise RuntimeError(f"source {name} has an unexpected kind")
        return str(existing["id"])
    created = request_json(base, "/api/v1/sources", token, "POST", {"name": name, "kind": kind, "endpoint": endpoint, "timezone": "UTC"})
    if str(created.get("kind", "")).lower() != kind.lower():
        raise RuntimeError(f"source {name} response has an unexpected kind")
    return str(created["id"])


def set_capacity(base: str, token: str, source: str, cap: int) -> None:
    for attempt in range(3):
        try:
            request_json(base, f"/api/v1/sources/{source}", token, "PATCH", {"maxConnections": cap})
            return
        except RuntimeError:
            if attempt == 2:
                raise
            time.sleep(3)


def run_gate(values: dict[str, str], build: bool = True) -> dict[str, Any]:
    def stop_handler(signum: int, _frame: Any) -> None:
        raise KeyboardInterrupt(f"received signal {signum}")

    signal.signal(signal.SIGTERM, stop_handler)
    signal.signal(signal.SIGINT, stop_handler)
    provider_url = values.get("URL", "").strip()
    username = values.get("USER", "")
    password = values.get("PWD", "")
    if not provider_url or not username or not password:
        raise RuntimeError("the Xtream environment file must define URL, USER, and PWD")
    xmltv_url = values.get("XMLTV_URL", values.get("IPTV_TEST_XMLTV_URL", "")).strip()
    xmltv_url = xmltv_url or xtream_endpoint(provider_url, username, password, "xmltv.php")
    try:
        cap = int(values.get("IPTV_TEST_PROVIDER_MAX_CONNECTIONS", values.get("LIVE_ACCEPTANCE_PROVIDER_CAP", "3")))
    except ValueError:
        raise RuntimeError("provider max connections must be an integer") from None
    if cap < 1:
        raise RuntimeError("provider max connections must be positive")
    temp_root = Path(tempfile.mkdtemp(prefix="iptv-live-input-"))
    xmltv_path = temp_root / "provider.xmltv"
    try:
        streams = xtream_live_streams(provider_url, username, password)
        for attempt in range(3):
            download_file(xmltv_url, xmltv_path)
            try:
                xmltv, provider_programmes, xmltv_names = xmltv_file(xmltv_path)
                break
            except RuntimeError:
                if attempt == 2:
                    raise
                time.sleep(2)
    except BaseException:
        shutil.rmtree(temp_root, ignore_errors=True)
        raise
    try:
        provider_ids = {str(item.get("epg_channel_id", "")).strip() for item in streams if str(item.get("epg_channel_id", "")).strip()}
        if not provider_ids:
            raise RuntimeError("Xtream get_live_streams returned no epg_channel_id values")
        samples = [{"channel": channel, "title": values[0][0], "start": values[0][1], "stop": values[0][2]} for channel, values in provider_programmes.items() if values][:3]
        identity_mode = "xtream-epg-channel-id"
        shared = provider_ids & xmltv
        if not shared:
            raise RuntimeError(f"Xtream and XMLTV have zero shared identities (xtreamEpgIds={len(provider_ids)}, xmltvIds={len(xmltv)}, xmltvNames={len(xmltv_names)}, mode={identity_mode})")
    except BaseException:
        shutil.rmtree(temp_root, ignore_errors=True)
        raise

    project = f"iptv-live-api-{secrets.token_hex(4)}"
    bootstrap, output_token, db_password = secret(), secret(), secret(24)
    master_key = base64.b64encode(secrets.token_bytes(32)).decode("ascii")
    gateway_port, postgres_port, core_port = free_port(), free_port(), free_port()
    env = {"POSTGRES_PASSWORD": db_password, "IPTV_GATEWAY_PORT": str(gateway_port), "IPTV_POSTGRES_PORT": str(postgres_port), "IPTV_PUBLIC_BASE_URL": f"http://127.0.0.1:{gateway_port}", "IPTV_OUTPUT_TOKEN": output_token, "IPTV_ADMIN_BOOTSTRAP_TOKEN": bootstrap, "IPTV_ADMIN_PASSWORD": secret(24), "IPTV_MASTER_KEY": master_key, "IPTV_WORKER_COUNT": "1"}
    handle, env_name = tempfile.mkstemp(prefix="iptv-live-api-", suffix=".env")
    Path(env_name).write_text("\n".join(f"{key}={value}" for key, value in env.items()) + "\n", encoding="utf-8")
    os.close(handle)
    env_file = Path(env_name)
    compose_override = temp_root / "compose.override.yml"
    compose_override.write_text(
        "services:\n"
        "  core:\n"
        "    ports:\n"
        f"      - 127.0.0.1:{core_port}:8081\n",
        encoding="utf-8",
    )
    global COMPOSE_OVERRIDE
    COMPOSE_OVERRIDE = compose_override
    base = f"http://127.0.0.1:{core_port}"
    cycles: list[dict[str, int]] = []
    identities: list[tuple[str, ...]] = []

    try:
        start_args = ["up", "-d", "--wait", "postgres", "core"]
        if build:
            start_args.insert(1, "--build")
        compose(env_file, project, *start_args)
        compose(env_file, project, "up", "-d", "worker")
        request_json(base, "/health/ready", bootstrap)
        baseline = storage_metrics(env_file, project)
        xtream_source_endpoint = xtream_endpoint(provider_url, username, password, "player_api.php")
        xtream_source = ensure_source(base, bootstrap, "live-api-acceptance-xtream", "Xtream", xtream_source_endpoint)
        xmltv_source = ensure_source(base, bootstrap, "live-api-acceptance-xmltv", "XMLTV", xmltv_url)
        set_capacity(base, bootstrap, xtream_source, cap)
        for cycle in range(1, 4):
            for source in (xtream_source, xmltv_source):
                try:
                    sync = request_json(base, f"/api/v1/sources/{source}/sync", bootstrap, "POST")
                except RuntimeError as error:
                    if "status=409" not in str(error):
                        raise
                    sync = {"jobId": "already-queued"}
                if not sync.get("jobId"):
                    raise RuntimeError("source sync response did not contain a job ID")
                wait_sync(base, bootstrap, source, time.monotonic() + 1800)
            # Refresh the provider guide sample for each cycle because live
            # schedules can advance while the three ingest cycles run.
            cycle_xmltv_path = temp_root / f"provider-{cycle}.xmltv"
            download_file(xmltv_url, cycle_xmltv_path)
            _cycle_ids, cycle_provider_programmes, _cycle_names = xmltv_file(cycle_xmltv_path)
            channels = all_pages(base, bootstrap, "/api/v1/channels")
            mappings = all_pages(base, bootstrap, "/api/v1/epg/mappings")
            # Verify that the gateway exposes programme rows without copying
            # the entire provider guide through the management API. The XMLTV
            # export below performs the complete programme sample comparison.
            programmes = page_items(request_json(base, "/api/v1/programmes?limit=500&offset=0", bootstrap))
            ids = [str(item.get("canonicalKey") or item.get("epgXmltvId") or "") for item in mappings]
            ids = [item for item in ids if item]
            if len(ids) != len(set(ids)):
                raise RuntimeError(f"cycle {cycle} produced duplicate channel identities")
            missing = shared - set(ids)
            if missing:
                raise RuntimeError(f"cycle {cycle} omitted {len(missing)} shared provider IDs")
            if not programmes:
                raise RuntimeError(f"cycle {cycle} produced no programme records")
            if not m3u_ids(output_playlist(base, output_token)):
                raise RuntimeError(f"cycle {cycle} produced an empty gateway output playlist")
            gateway_xmltv = temp_root / f"gateway-{cycle}.xmltv"
            request = urllib.request.Request(f"{base}/out/{output_token}/xmltv.xml")
            try:
                with urllib.request.urlopen(request, timeout=30) as response, gateway_xmltv.open("wb") as output:
                    shutil.copyfileobj(response, output, length=1024 * 1024)
            except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, OSError):
                raise RuntimeError("gateway XMLTV request failed") from None
            gateway_ids, gateway_programmes, _gateway_names = xmltv_file(gateway_xmltv)
            if not gateway_ids or not gateway_programmes:
                raise RuntimeError(f"cycle {cycle} produced no gateway XMLTV records")
            selected_provider = next(
                ((channel, values[0]) for channel, values in cycle_provider_programmes.items() if values and channel in shared),
                None,
            )
            if selected_provider:
                selected_channel, selected_tuple = selected_provider
                mapped_channel = next(
                    (
                        str(item.get("channelId"))
                        for item in mappings
                        if str(item.get("canonicalKey") or "") == selected_channel
                    ),
                    "",
                )
                gateway_samples = gateway_programmes.get(mapped_channel, [])
                if not mapped_channel or mapped_channel not in gateway_ids:
                    raise RuntimeError(f"cycle {cycle} omitted gateway channel mapping for {selected_channel}")
                if not any(programme_key(value) == programme_key(selected_tuple) for value in gateway_samples):
                    raise RuntimeError(f"cycle {cycle} did not map the current provider programme sample")
            # Providers can add or remove non-guide streams while a run is
            # active. Require stable identity for the shared guide set that
            # this acceptance gate uses for EPG mapping.
            identities.append(tuple(sorted(set(ids) & shared)))
            cycles.append(storage_metrics(env_file, project))
        if identities[0] != identities[1] or identities[1] != identities[2]:
            raise RuntimeError("shared channel identities changed between sync cycles")
        final = storage_metrics(env_file, project)
        if any(final.get(key, 0) != 0 for key in ("pendingJobs", "reconciliationCandidates", "stagingSnapshots")):
            raise RuntimeError(
                "terminal live acceptance state retained ingest resources: "
                f"pendingJobs={final.get('pendingJobs', 0)}, "
                f"reconciliationCandidates={final.get('reconciliationCandidates', 0)}, "
                f"stagingSnapshots={final.get('stagingSnapshots', 0)}"
            )
        peak = max((item.get("databaseBytes", 0) for item in [baseline, *cycles, final]), default=0)
        report = {"identityMode": identity_mode, "sharedProviderIds": len(shared), "providerUnmatched": {"xtreamOnlyCount": len(provider_ids - xmltv), "xmltvOnlyCount": len(xmltv - provider_ids), "xtreamOnlySample": sorted(provider_ids - xmltv)[:10], "xmltvOnlySample": sorted(xmltv - provider_ids)[:10]}, "providerCap": cap, "cycles": 3, "gatewayOutput": "playlist verified", "sampleProgrammes": samples, "storage": {"baseline": baseline, "cycles": cycles, "peakDatabaseBytes": peak, "final": final}}
        print(json.dumps(report, sort_keys=True), flush=True)
        return report
    finally:
        # Ignore a second interrupt while runner-owned resources are removed.
        # The first signal already stopped the acceptance loop; cleanup must
        # finish so containers, volumes, and temporary files cannot linger.
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        time.sleep(10)
        cleanup = compose(env_file, project, "down", "--volumes", "--remove-orphans", check=False)
        remaining = compose(env_file, project, "ps", "-q", check=False)
        for _attempt in range(24):
            if cleanup.returncode == 0 and remaining.returncode == 0 and not remaining.stdout.strip():
                break
            time.sleep(5)
            cleanup = compose(env_file, project, "down", "--volumes", "--remove-orphans", check=False)
            remaining = compose(env_file, project, "ps", "-q", check=False)
        env_file.unlink(missing_ok=True)
        shutil.rmtree(temp_root, ignore_errors=True)
        COMPOSE_OVERRIDE = None
        if cleanup.returncode or remaining.returncode or remaining.stdout.strip():
            detail = (cleanup.stderr or cleanup.stdout or remaining.stderr or remaining.stdout).strip().splitlines()
            message = f"live acceptance cleanup failed: {'\\n'.join(detail[-12:])}"
            if sys.exc_info()[1] is None:
                raise RuntimeError(message)
            print(message, file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description="Run real-provider API acceptance")
    parser.add_argument("--env-file", default=str(ROOT / ".env.xtreme"))
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--no-build", action="store_true", help="use the existing local core image")
    args = parser.parse_args()
    try:
        if args.self_test:
            assert m3u_ids(b'#EXTINF:-1 tvg-id="ABC",Test\nurl\n') == {"ABC"}
            print("live API acceptance self-test passed")
        else:
            run_gate(parse_env(Path(args.env_file)), build=not args.no_build)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as exc:
        print(f"live API acceptance failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
