#!/usr/bin/env python3
"""Run the real-provider API acceptance gate with the Python standard library."""

from __future__ import annotations

import argparse
import base64
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
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
ALLOWED_ENV = {"IPTV_TEST_M3U_URL", "IPTV_TEST_XMLTV_URL", "IPTV_TEST_PROVIDER_MAX_CONNECTIONS", "LIVE_ACCEPTANCE_PROVIDER_CAP"}
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
            return json.loads(response.read().decode("utf-8"))
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, json.JSONDecodeError):
        raise RuntimeError(f"gateway request failed: {method} {path}") from None


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
                match = re.search(r'(?:tvg-id|tvgid)=["\']?([^"\' ,]+)', line, re.I)
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
    command = ["docker-compose", "--parallel", "1", "--project-name", project, "--env-file", str(env_file), *args]
    inherited = {key: value for key, value in os.environ.items() if not (key.startswith("IPTV_") or key in {"POSTGRES_PASSWORD", "COMPOSE_PROJECT_NAME", "COMPOSE_PARALLEL_LIMIT", "CARGO_BUILD_JOBS"})}
    inherited["COMPOSE_PARALLEL_LIMIT"] = "1"
    inherited["CARGO_BUILD_JOBS"] = "1"
    return subprocess.run(command, cwd=ROOT, env=inherited, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=check)


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
    request_json(base, f"/api/v1/sources/{source}", token, "PATCH", {"maxConnections": cap})


def run_gate(values: dict[str, str]) -> dict[str, Any]:
    def stop_handler(signum: int, _frame: Any) -> None:
        raise KeyboardInterrupt(f"received signal {signum}")

    signal.signal(signal.SIGTERM, stop_handler)
    signal.signal(signal.SIGINT, stop_handler)
    m3u_url, xmltv_url = values.get("IPTV_TEST_M3U_URL", "").strip(), values.get("IPTV_TEST_XMLTV_URL", "").strip()
    if not m3u_url or not xmltv_url:
        raise RuntimeError(".env.live must define IPTV_TEST_M3U_URL and IPTV_TEST_XMLTV_URL")
    try:
        cap = int(values.get("IPTV_TEST_PROVIDER_MAX_CONNECTIONS", values.get("LIVE_ACCEPTANCE_PROVIDER_CAP", "3")))
    except ValueError:
        raise RuntimeError("provider max connections must be an integer") from None
    if cap < 1:
        raise RuntimeError("provider max connections must be positive")
    temp_root = Path(tempfile.mkdtemp(prefix="iptv-live-input-"))
    m3u_path, xmltv_path = temp_root / "provider.m3u", temp_root / "provider.xmltv"
    try:
        download_file(m3u_url, m3u_path)
        download_file(xmltv_url, xmltv_path)
    except BaseException:
        shutil.rmtree(temp_root, ignore_errors=True)
        raise
    try:
        m3u, m3u_names = m3u_ids_file(m3u_path), m3u_names_file(m3u_path)
        xmltv, provider_programmes, xmltv_names = xmltv_file(xmltv_path)
        samples = [{"channel": channel, "title": values[0][0], "start": values[0][1], "stop": values[0][2]} for channel, values in provider_programmes.items() if values][:3]
        identity_mode = "provider-id"
        if not m3u:
            identity_mode = "tvg-name-fallback"
            shared = {name.casefold() for name in m3u_names} & {name.casefold() for name in xmltv_names}
        else:
            shared = m3u & xmltv
        if not shared:
            raise RuntimeError(
                f"M3U and XMLTV have zero shared identities (m3uIds={len(m3u)}, "
                f"m3uNames={len(m3u_names)}, xmltvIds={len(xmltv)}, xmltvNames={len(xmltv_names)}, mode={identity_mode})"
            )
    except BaseException:
        shutil.rmtree(temp_root, ignore_errors=True)
        raise

    project = f"iptv-live-api-{secrets.token_hex(4)}"
    bootstrap, output_token, password = secret(), secret(), secret(24)
    master_key = base64.b64encode(secrets.token_bytes(32)).decode("ascii")
    gateway_port, postgres_port = free_port(), free_port()
    env = {"POSTGRES_PASSWORD": password, "IPTV_GATEWAY_PORT": str(gateway_port), "IPTV_POSTGRES_PORT": str(postgres_port), "IPTV_PUBLIC_BASE_URL": f"http://127.0.0.1:{gateway_port}", "IPTV_OUTPUT_TOKEN": output_token, "IPTV_ADMIN_BOOTSTRAP_TOKEN": bootstrap, "IPTV_ADMIN_PASSWORD": "", "IPTV_MASTER_KEY": master_key, "IPTV_WORKER_COUNT": "2"}
    handle, env_name = tempfile.mkstemp(prefix="iptv-live-api-", suffix=".env")
    Path(env_name).write_text("\n".join(f"{key}={value}" for key, value in env.items()) + "\n", encoding="utf-8")
    os.close(handle)
    env_file = Path(env_name)
    base = f"http://127.0.0.1:{gateway_port}"
    cycles: list[dict[str, int]] = []
    identities: list[tuple[str, ...]] = []

    try:
        compose(env_file, project, "up", "--build", "-d", "--wait", "postgres", "core", "worker", "web", "gateway")
        request_json(base, "/health/ready", bootstrap)
        baseline = storage_metrics(env_file, project)
        m3u_source = ensure_source(base, bootstrap, "live-api-acceptance-m3u", "M3U", m3u_url)
        xmltv_source = ensure_source(base, bootstrap, "live-api-acceptance-xmltv", "XMLTV", xmltv_url)
        set_capacity(base, bootstrap, m3u_source, cap)
        for cycle in range(1, 4):
            for source in (m3u_source, xmltv_source):
                sync = request_json(base, f"/api/v1/sources/{source}/sync", bootstrap, "POST")
                if not sync.get("jobId"):
                    raise RuntimeError("source sync response did not contain a job ID")
                wait_sync(base, bootstrap, source, time.monotonic() + 1800)
            channels = all_pages(base, bootstrap, "/api/v1/channels")
            mappings = all_pages(base, bootstrap, "/api/v1/epg/mappings")
            programmes = all_pages(base, bootstrap, "/api/v1/programmes")
            if identity_mode == "tvg-name-fallback":
                ids = [str(item.get("channelName") or "") for item in mappings]
            else:
                ids = [str(item.get("canonicalKey") or item.get("epgXmltvId") or "") for item in mappings]
            ids = [item for item in ids if item]
            if identity_mode == "tvg-name-fallback":
                ids = [item.casefold() for item in ids]
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
            gateway_ids, gateway_programmes = xmltv_file(gateway_xmltv)
            if not gateway_ids or not gateway_programmes:
                raise RuntimeError(f"cycle {cycle} produced no gateway XMLTV records")
            selected_provider = next((item for item in provider_programmes.items() if item[1]), None)
            if selected_provider:
                selected_tuple = selected_provider[1][0]
                if not any(values and values[0] == selected_tuple for values in gateway_programmes.values()):
                    raise RuntimeError(f"cycle {cycle} changed the selected programme sample")
            identities.append(tuple(sorted(ids)))
            cycles.append(storage_metrics(env_file, project))
        if identities[0] != identities[1] or identities[1] != identities[2]:
            raise RuntimeError("channel identities changed between sync cycles")
        final = storage_metrics(env_file, project)
        peak = max((item.get("databaseBytes", 0) for item in [baseline, *cycles, final]), default=0)
        report = {"identityMode": identity_mode, "sharedProviderIds": len(shared), "providerUnmatched": {"m3uOnlyCount": len(m3u - xmltv), "xmltvOnlyCount": len(xmltv - m3u), "m3uOnlySample": sorted(m3u - xmltv)[:10], "xmltvOnlySample": sorted(xmltv - m3u)[:10]}, "providerCap": cap, "cycles": 3, "gatewayOutput": "playlist verified", "sampleProgrammes": samples, "storage": {"baseline": baseline, "cycles": cycles, "peakDatabaseBytes": peak, "final": final}}
        print(json.dumps(report, sort_keys=True))
        return report
    finally:
        time.sleep(2)
        cleanup = compose(env_file, project, "down", "--volumes", "--remove-orphans", check=False)
        remaining = compose(env_file, project, "ps", "-q", check=False)
        env_file.unlink(missing_ok=True)
        shutil.rmtree(temp_root, ignore_errors=True)
        if cleanup.returncode or remaining.stdout.strip():
            raise RuntimeError("live acceptance cleanup failed")


def main() -> int:
    parser = argparse.ArgumentParser(description="Run real-provider API acceptance")
    parser.add_argument("--env-file", default=str(ROOT / ".env.live"))
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    try:
        if args.self_test:
            assert m3u_ids(b'#EXTINF:-1 tvg-id="ABC",Test\nurl\n') == {"ABC"}
            print("live API acceptance self-test passed")
        else:
            run_gate(parse_env(Path(args.env_file)))
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as exc:
        print(f"live API acceptance failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
