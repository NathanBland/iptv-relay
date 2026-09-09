#!/usr/bin/env python3
"""Compare real M3U and Xtream streams and test both through the local API."""

from __future__ import annotations

import argparse
import concurrent.futures
import gzip
import hashlib
import http.client
import json
import os
import re
import secrets
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
TERMINAL = {"succeeded", "failed", "cancelled", "dead"}
MPEG_TS_PACKET = 188
MPEG_TS_SAMPLE = MPEG_TS_PACKET * 50


def parse_env(path: Path, allowed: set[str]) -> dict[str, str]:
    """Parse simple KEY=value records without evaluating shell syntax."""
    values: dict[str, str] = {}
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        match = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)=(.*)", line)
        if not match:
            raise ValueError(f"invalid environment record on line {number}")
        key, value = match.groups()
        if key not in allowed:
            continue
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
            value = value[1:-1]
        values[key] = value
    return values


def normalize_name(value: str) -> str:
    """Normalize a provider name for fallback matching."""
    return re.sub(r"[^a-z0-9]+", "", value.casefold())


def parse_targets(values: dict[str, str]) -> tuple[str, ...]:
    """Read the exact provider names to compare from the test environment."""
    targets = tuple(item.strip() for item in values.get("IPTV_TEST_TARGETS", "").split("|") if item.strip())
    normalized = tuple(normalize_name(item) for item in targets)
    if len(targets) != 2:
        raise RuntimeError("IPTV_TEST_TARGETS must contain exactly two pipe-separated provider names")
    if any(not item for item in normalized) or len(set(normalized)) != len(normalized):
        raise RuntimeError("IPTV_TEST_TARGETS must contain unique provider names")
    return targets


def safe_url_shape(value: str) -> str:
    """Return a credential-free URL shape suitable for a report."""
    parsed = urllib.parse.urlsplit(value)
    segments = [segment for segment in parsed.path.split("/") if segment]
    if len(segments) >= 4 and segments[-4].casefold() == "live":
        path = "/live/<username>/<password>/" + segments[-1]
    else:
        path = "/" + "/".join("<path>" for _ in segments)
    return f"{parsed.scheme}://<provider>{path}"


def url_digest(value: str) -> str:
    """Return a stable credential-free comparison value for one URL."""
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def stream_id_from_url(value: str) -> str:
    match = re.search(r"/live/[^/]+/[^/]+/(\d+)\.(?:ts|m3u8)(?:\?|$)", value, re.I)
    return match.group(1) if match else ""


def xtream_base(value: str) -> str:
    root = value.rstrip("/")
    for suffix in ("/player_api.php", "/xmltv.php"):
        if root.casefold().endswith(suffix):
            root = root[: -len(suffix)]
    return root


def xtream_endpoint(base: str, username: str, password: str, action: str | None = None) -> str:
    query = {"username": username, "password": password}
    if action:
        query["action"] = action
    return f"{xtream_base(base)}/player_api.php?{urllib.parse.urlencode(query)}"


def xtream_stream_url(base: str, username: str, password: str, stream_id: int) -> str:
    return f"{xtream_base(base)}/live/{urllib.parse.quote(username, safe='')}/{urllib.parse.quote(password, safe='')}/{stream_id}.ts"


def download(url: str, destination: Path) -> None:
    request = urllib.request.Request(url, headers={"Accept-Encoding": "gzip", "User-Agent": "iptv-relay-provider-compare/1"})
    try:
        with urllib.request.urlopen(request, timeout=180) as response, destination.open("wb") as output:
            source = gzip.GzipFile(fileobj=response) if response.headers.get("Content-Encoding", "").lower() == "gzip" else response
            with source:
                shutil.copyfileobj(source, output, length=1024 * 1024)
    except (OSError, urllib.error.HTTPError, urllib.error.URLError, TimeoutError):
        raise RuntimeError("provider document download failed") from None


def fetch_json(url: str) -> Any:
    request = urllib.request.Request(url, headers={"Accept": "application/json", "User-Agent": "iptv-relay-provider-compare/1"})
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            return json.loads(response.read().decode("utf-8-sig"))
    except (OSError, urllib.error.HTTPError, urllib.error.URLError, TimeoutError, json.JSONDecodeError):
        raise RuntimeError("Xtream API request failed") from None


def parse_m3u(path: Path, targets: tuple[str, ...]) -> dict[str, list[dict[str, str]]]:
    records: dict[str, list[dict[str, str]]] = {target: [] for target in targets}
    target_keys = {normalize_name(target): target for target in targets}
    pending: tuple[str, dict[str, str]] | None = None
    with path.open("r", encoding="utf-8-sig", errors="replace") as stream:
        for raw in stream:
            line = raw.rstrip("\r\n")
            if line.startswith("#EXTINF:"):
                attrs = dict(re.findall(r'''([\w-]+)=["']([^"']*)["']''', line))
                title = line.rsplit(",", 1)[-1].strip()
                target = target_keys.get(normalize_name(title))
                pending = (target, attrs) if target else None
            elif pending and line and not line.startswith("#"):
                target, attrs = pending
                records[target].append({
                    "title": attrs.get("tvg-name", "") or target,
                    "tvgId": attrs.get("tvg-id", ""),
                    "group": attrs.get("group-title", ""),
                    "url": line,
                    "streamId": stream_id_from_url(line),
                })
                pending = None
    return records


def fetch_provider_data(values_live: dict[str, str], values_xtream: dict[str, str], targets: tuple[str, ...], temp_root: Path, m3u_cache: Path | None = None, xtream_cache: Path | None = None) -> tuple[dict[str, list[dict[str, str]]], dict[str, list[dict[str, Any]]]]:
    m3u_url = values_live.get("IPTV_TEST_M3U_URL", "").strip()
    base = values_xtream.get("URL", "").strip()
    username = values_xtream.get("USER", "")
    password = values_xtream.get("PWD", "")
    if not m3u_url or not base or not username or not password:
        raise RuntimeError("both environment files must contain their required provider values")
    m3u_path = temp_root / "provider.m3u"
    if m3u_cache:
        shutil.copyfile(m3u_cache, m3u_path)
    else:
        download(m3u_url, m3u_path)
    m3u = parse_m3u(m3u_path, targets)
    if xtream_cache:
        payload = json.loads(xtream_cache.read_text(encoding="utf-8-sig"))
    else:
        auth = fetch_json(xtream_endpoint(base, username, password))
        user_info = auth.get("user_info", {}) if isinstance(auth, dict) else {}
        if not isinstance(user_info, dict) or str(user_info.get("auth", "0")).casefold() not in {"1", "true"}:
            raise RuntimeError("Xtream authentication was rejected")
        payload = fetch_json(xtream_endpoint(base, username, password, "get_live_streams"))
    if not isinstance(payload, list) or not all(isinstance(item, dict) for item in payload):
        raise RuntimeError("Xtream live-stream response was invalid")
    xtream: dict[str, list[dict[str, Any]]] = {target: [] for target in targets}
    target_keys = {normalize_name(target): target for target in targets}
    for item in payload:
        target = target_keys.get(normalize_name(str(item.get("name", ""))))
        if target:
            xtream[target].append(item)
    return m3u, xtream


def pair_record(m3u_record: dict[str, str], xtream_record: dict[str, Any], values_xtream: dict[str, str]) -> dict[str, Any]:
    xtream_id = int(xtream_record.get("stream_id", 0))
    if xtream_id <= 0:
        raise RuntimeError("Xtream record has no valid stream ID")
    xtream_url = xtream_stream_url(values_xtream["URL"], values_xtream["USER"], values_xtream["PWD"], xtream_id)
    return {
        "m3u": m3u_record,
        "xtream": xtream_record,
        "xtreamUrl": xtream_url,
        "sameStreamId": m3u_record["streamId"] == str(xtream_id),
        "sameUrlShape": safe_url_shape(m3u_record["url"]) == safe_url_shape(xtream_url),
        "sameExactUrl": m3u_record["url"] == xtream_url,
        "m3uUrlDigest": url_digest(m3u_record["url"]),
        "xtreamUrlDigest": url_digest(xtream_url),
    }


def choose_records(m3u: dict[str, list[dict[str, str]]], xtream: dict[str, list[dict[str, Any]]], targets: tuple[str, ...], values_xtream: dict[str, str]) -> dict[str, dict[str, Any]]:
    selected: dict[str, dict[str, Any]] = {}
    for target in targets:
        if not m3u[target]:
            raise RuntimeError(f"M3U has no record for {target}")
        if not xtream[target]:
            raise RuntimeError(f"Xtream has no record for {target}")
        pairs = [
            (m3u_record, xtream_record)
            for m3u_record in m3u[target]
            for xtream_record in xtream[target]
            if m3u_record["streamId"] and m3u_record["streamId"] == str(xtream_record.get("stream_id", ""))
        ]
        if not pairs:
            pairs = [(m3u[target][0], xtream[target][0])]
        selected[target] = pair_record(*pairs[0], values_xtream)
        selected[target]["candidatePairs"] = [pair_record(m3u_record, xtream_record, values_xtream) for m3u_record, xtream_record in pairs]
    return selected


def probe_stream(url: str) -> dict[str, Any]:
    try:
        request = urllib.request.Request(url, headers={"User-Agent": "iptv-relay-provider-compare/1"})
        with urllib.request.urlopen(request, timeout=45) as response:
            data = response.read(MPEG_TS_SAMPLE)
        complete = len(data) >= MPEG_TS_PACKET and all(data[index] == 0x47 for index in range(0, len(data) - MPEG_TS_PACKET + 1, MPEG_TS_PACKET))
        return {"bytes": len(data), "mpegTs": complete, "status": "ok" if complete else "bad-mpegts"}
    except (OSError, urllib.error.HTTPError, urllib.error.URLError, TimeoutError, http.client.HTTPException) as error:
        return {"status": type(error).__name__}


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def token(length: int = 48) -> str:
    return secrets.token_urlsafe(length)


def compose(env_file: Path, project: str, override: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    command = ["docker-compose", "--parallel", "1", "-f", str(ROOT / "docker-compose.yml"), "-f", str(override), "--project-name", project, "--env-file", str(env_file), *args]
    inherited = {key: value for key, value in os.environ.items() if not (key.startswith("IPTV_") or key in {"POSTGRES_PASSWORD", "COMPOSE_PROJECT_NAME", "COMPOSE_PARALLEL_LIMIT", "CARGO_BUILD_JOBS"})}
    inherited.update({"COMPOSE_PARALLEL_LIMIT": "1", "CARGO_BUILD_JOBS": "1", "DOCKER_BUILDKIT": "1"})
    result = subprocess.run(command, cwd=ROOT, env=inherited, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if check and result.returncode:
        detail = (result.stderr or result.stdout).strip().splitlines()
        raise RuntimeError(f"Compose command failed ({' '.join(args)}): {' '.join(detail[-12:])}")
    return result


def api_json(base: str, path: str, admin_token: str, method: str = "GET", body: Any = None) -> Any:
    headers = {"Authorization": f"Bearer {admin_token}", "Accept": "application/json"}
    data = None
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            payload = response.read()
            return None if not payload else json.loads(payload.decode("utf-8"))
    except urllib.error.HTTPError as error:
        detail = error.read(512).decode("utf-8", errors="replace").replace("\n", " ")
        raise RuntimeError(f"API request failed: {method} {path} status={error.code} detail={detail}") from None
    except (OSError, urllib.error.URLError, TimeoutError, json.JSONDecodeError):
        raise RuntimeError(f"API request failed: {method} {path}") from None


def wait_ready(base: str, token_value: str) -> None:
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:
        try:
            api_json(base, "/health/ready", token_value)
            return
        except RuntimeError:
            time.sleep(2)
    raise RuntimeError("local API did not become ready")


def wait_sync(base: str, token_value: str, source_id: str) -> dict[str, Any]:
    deadline = time.monotonic() + 1800
    while time.monotonic() < deadline:
        try:
            status = api_json(base, f"/api/v1/sources/{source_id}/sync-status", token_value)
        except RuntimeError:
            time.sleep(2)
            continue
        state = str(status.get("status", "")).casefold()
        if state in TERMINAL:
            if state != "succeeded":
                message = str(status.get("message", "")).replace("\n", " ")
                stage = str(status.get("stage", ""))
                detail = ""
                try:
                    jobs = api_json(base, "/api/v1/jobs", token_value)
                    for job in jobs if isinstance(jobs, list) else []:
                        if str(job.get("id", "")) == str(status.get("jobId", "")):
                            detail = str(job.get("lastError", "")).replace("\n", " ")
                            break
                except RuntimeError:
                    pass
                raise RuntimeError(f"source sync ended with {state} stage={stage} message={message[:240]} error={detail[:240]}")
            return status
        time.sleep(2)
    raise RuntimeError("source sync exceeded its deadline")


def set_capacity(base: str, token_value: str, source_id: str, capacity: int) -> None:
    api_json(base, f"/api/v1/sources/{source_id}", token_value, "PATCH", {"maxConnections": capacity})


def create_source(base: str, token_value: str, kind: str, endpoint: str, values_xtream: dict[str, str], name: str) -> str:
    if kind == "M3U":
        body = {"name": name, "kind": kind, "endpoint": endpoint, "timezone": "UTC"}
    else:
        body = {"name": name, "kind": kind, "serverUrl": xtream_base(values_xtream["URL"]), "username": values_xtream["USER"], "password": values_xtream["PWD"], "timezone": "UTC"}
    created = api_json(base, "/api/v1/sources", token_value, "POST", body)
    return str(created["id"])


def target_channels(base: str, token_value: str, targets: tuple[str, ...]) -> dict[str, str]:
    output: dict[str, str] = {}
    for target in targets:
        query = urllib.parse.urlencode({"search": target, "limit": 50, "offset": 0})
        body = api_json(base, f"/api/v1/channels?{query}", token_value)
        items = body.get("items", []) if isinstance(body, dict) else []
        exact = [item for item in items if normalize_name(str(item.get("name", ""))) == normalize_name(target)]
        chosen = exact[0] if exact else (items[0] if items else None)
        if not chosen:
            raise RuntimeError(f"local catalog has no channel for {target}")
        output[target] = str(chosen["id"])
    return output


def set_adapter(env_file: Path, project: str, override: Path, source_name: str, adapter: str) -> None:
    escaped_name = source_name.replace("'", "''")
    sql = f"UPDATE provider_accounts SET input_adapter = '{adapter}' WHERE name = '{escaped_name}';"
    compose(env_file, project, override, "exec", "-T", "postgres", "psql", "-U", "iptv", "-d", "iptv", "-v", "ON_ERROR_STOP=1", "-c", sql)


def stream_through_api(base: str, token_value: str, channel_id: str, seconds: int) -> dict[str, Any]:
    request = urllib.request.Request(base + f"/api/v1/channels/{channel_id}/stream", headers={"Authorization": f"Bearer {token_value}", "User-Agent": "iptv-relay-provider-compare/1"})
    try:
        with urllib.request.urlopen(request, timeout=90) as response:
            data = bytearray()
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                try:
                    chunk = response.read(MPEG_TS_SAMPLE)
                except http.client.IncompleteRead as error:
                    chunk = error.partial
                if not chunk:
                    break
                data.extend(chunk)
            complete = len(data) >= MPEG_TS_PACKET and all(data[index] == 0x47 for index in range(0, len(data) - MPEG_TS_PACKET + 1, MPEG_TS_PACKET))
        return {"bytes": len(data), "mpegTs": complete, "durationSeconds": seconds, "status": "ok" if complete else "bad-mpegts"}
    except urllib.error.HTTPError as error:
        detail = error.read(512).decode("utf-8", errors="replace").replace("\n", " ")
        return {"status": f"http-{error.code}", "detail": detail[:240]}
    except (OSError, urllib.error.URLError, TimeoutError, http.client.HTTPException) as error:
        return {"status": type(error).__name__}


def storage_metrics(env_file: Path, project: str, override: Path) -> dict[str, int]:
    query = "SELECT pg_database_size(current_database()), (SELECT count(*) FROM source_snapshots WHERE status='staging'), (SELECT count(*) FROM provider_reconciliation_candidates), (SELECT count(*) FROM jobs WHERE status IN ('queued','running'));"
    result = compose(env_file, project, override, "exec", "-T", "postgres", "psql", "-U", "iptv", "-d", "iptv", "-At", "-c", query, check=False)
    fields = result.stdout.strip().split("|")
    if result.returncode or len(fields) != 4:
        raise RuntimeError("local storage metrics unavailable")
    return {"databaseBytes": int(fields[0]), "stagingSnapshots": int(fields[1]), "reconciliationCandidates": int(fields[2]), "pendingJobs": int(fields[3])}


def self_test() -> None:
    """Run credential-free checks for URL comparison helpers."""
    targets = tuple(f"target-{index}" for index in range(2))
    assert parse_targets({"IPTV_TEST_TARGETS": " | ".join(targets)}) == targets
    sample = "http://provider.test/live/user/password/13599.ts"
    assert stream_id_from_url(sample) == "13599"
    assert safe_url_shape(sample) == "http://<provider>/live/<username>/<password>/13599.ts"
    assert url_digest(sample) == hashlib.sha256(sample.encode("utf-8")).hexdigest()
    m3u_record = {"title": targets[0], "tvgId": "", "group": "Sports", "url": sample, "streamId": "13599"}
    xtream_record = {"name": targets[0], "stream_id": 13599}
    pair = pair_record(m3u_record, xtream_record, {"URL": "http://provider.test", "USER": "user", "PWD": "password"})
    assert pair["sameExactUrl"]
    print("provider stream comparison self-test passed")


def run_stack(kind: str, selected: dict[str, dict[str, Any]], targets: tuple[str, ...], values_xtream: dict[str, str], capacity: int, root: Path, build: bool, stream_seconds: int) -> dict[str, Any]:
    project = f"iptv-stream-compare-{kind.casefold()}-{secrets.token_hex(4)}"
    env_file = root / f"{kind.casefold()}.env"
    override = root / f"{kind.casefold()}.compose.yml"
    admin = token()
    output = token()
    database_password = token(24)
    env_file.write_text("\n".join([
        f"POSTGRES_PASSWORD={database_password}",
        "IPTV_GATEWAY_BIND=127.0.0.1",
        f"IPTV_GATEWAY_PORT={free_port()}",
        f"IPTV_POSTGRES_PORT={free_port()}",
        f"IPTV_PUBLIC_BASE_URL=http://127.0.0.1:{free_port()}",
        f"IPTV_OUTPUT_TOKEN={output}",
        f"IPTV_ADMIN_BOOTSTRAP_TOKEN={admin}",
        f"IPTV_ADMIN_PASSWORD={token(24)}",
        "IPTV_MASTER_KEY=AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
        "IPTV_WORKER_COUNT=1",
        "IPTV_TUNER_COUNT=3",
        "RUST_LOG=info",
    ]) + "\n", encoding="utf-8")
    fixture_service = ""
    if kind == "M3U":
        mini_path = root / "selected.m3u"
        lines = ["#EXTM3U"]
        for target in targets:
            record = selected[target]["m3u"]
            lines.append(f'#EXTINF:-1 tvg-id="{record["tvgId"]}" tvg-name="{record["title"]}" group-title="{record["group"]}",{record["title"]}')
            lines.append(record["url"])
        mini_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        fixture_service = (
            "  provider-fixture:\n"
            "    image: caddy:2.10-alpine\n"
            '    command: ["caddy", "file-server", "--root", "/data", "--listen", ":8090"]\n'
            f"    volumes:\n      - {root}:/data:ro\n"
        )
    override.write_text(
        "services:\n"
        "  core:\n"
        "    ports:\n"
        f"      - 127.0.0.1:{free_port()}:8081\n"
        + fixture_service,
        encoding="utf-8",
    )
    # Read the actual published core port from the override rather than rely on
    # the generated environment's unused gateway port.
    core_port = int(re.search(r"127\.0\.0\.1:(\d+):8081", override.read_text(encoding="utf-8")).group(1))
    base = f"http://127.0.0.1:{core_port}"
    source_name = f"stream-compare-{kind.casefold()}"
    try:
        initial_services = ["postgres", "core"] + (["provider-fixture"] if kind == "M3U" else [])
        start = ["up"] + (["--build"] if build else []) + ["-d", "--wait", *initial_services]
        compose(env_file, project, override, *start)
        wait_ready(base, admin)
        compose(env_file, project, override, "up", "-d", "--wait", "worker")
        if kind == "M3U":
            fixture_id_result = compose(env_file, project, override, "ps", "-q", "provider-fixture", check=False)
            fixture_id = fixture_id_result.stdout.strip()
            if fixture_id_result.returncode or not fixture_id:
                raise RuntimeError("the M3U fixture container did not start")
            fixture_ip_result = subprocess.run(
                ["docker", "inspect", "-f", "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}", fixture_id],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )
            fixture_ip = fixture_ip_result.stdout.strip()
            if fixture_ip_result.returncode or not fixture_ip:
                raise RuntimeError("the M3U fixture container has no private address")
            endpoint = f"http://{fixture_ip}:8090/selected.m3u"
        else:
            endpoint = xtream_endpoint(values_xtream["URL"], values_xtream["USER"], values_xtream["PWD"])
        if kind == "M3U":
            fixture_check = compose(env_file, project, override, "exec", "-T", "core", "curl", "-fsS", endpoint, check=False)
            if fixture_check.returncode:
                detail = (fixture_check.stderr or fixture_check.stdout).strip().replace("\n", " ")
                raise RuntimeError(f"the core container cannot fetch the local M3U fixture: {detail[:240]}")
        source_id = create_source(base, admin, kind, endpoint, values_xtream, source_name)
        set_capacity(base, admin, source_id, capacity)
        source_rows = api_json(base, "/api/v1/sources", admin)
        source_summary = next((item for item in source_rows if str(item.get("id")) == source_id), {})
        sync = wait_sync(base, admin, source_id)
        channels = target_channels(base, admin, targets)
        adapters: dict[str, Any] = {}
        for adapter in ("auto", "ffmpeg", "vlc"):
            set_adapter(env_file, project, override, source_name, adapter)
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
                futures = {target: executor.submit(stream_through_api, base, admin, channels[target], stream_seconds) for target in targets}
                adapters[adapter] = {target: futures[target].result() for target in targets}
        return {"source": kind, "sourceMaxConnections": source_summary.get("maxConnections"), "sync": {"status": sync.get("status"), "recordsProcessed": sync.get("recordsProcessed")}, "channels": channels, "adapters": adapters, "storage": storage_metrics(env_file, project, override)}
    finally:
        down = compose(env_file, project, override, "down", "--volumes", "--remove-orphans", check=False)
        remaining = compose(env_file, project, override, "ps", "-aq", check=False)
        if down.returncode or remaining.stdout.strip():
            raise RuntimeError(f"cleanup failed for {kind} comparison stack")


def main() -> int:
    parser = argparse.ArgumentParser(description="Compare real M3U and Xtream streams and test local playback")
    parser.add_argument("--live-env", default=str(ROOT / ".env.live"))
    parser.add_argument("--xtream-env", default=str(ROOT / ".env.xtreme"))
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--m3u-cache", type=Path)
    parser.add_argument("--xtream-cache", type=Path)
    parser.add_argument("--connection-cap", type=int)
    parser.add_argument("--source", choices=("M3U", "Xtream", "both"), default="both")
    parser.add_argument("--stream-seconds", type=int, default=10)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--report-file", type=Path)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    temp_root = ROOT / f".provider-stream-compare-{secrets.token_hex(4)}"
    temp_root.mkdir()
    try:
        live = parse_env(Path(args.live_env), {"IPTV_TEST_M3U_URL", "IPTV_TEST_PROVIDER_MAX_CONNECTIONS", "IPTV_TEST_TARGETS"})
        xtream = parse_env(Path(args.xtream_env), {"URL", "USER", "PWD"})
        target_values = {"IPTV_TEST_TARGETS": live.get("IPTV_TEST_TARGETS", os.environ.get("IPTV_TEST_TARGETS", ""))}
        targets = parse_targets(target_values)
        selected_data, xtream_data = fetch_provider_data(live, xtream, targets, temp_root, args.m3u_cache, args.xtream_cache)
        selected = choose_records(selected_data, xtream_data, targets, xtream)
        direct: dict[str, Any] = {}
        candidate_direct: dict[str, list[dict[str, Any]]] = {}
        for target in targets:
            attempts: list[dict[str, Any]] = []
            working: dict[str, Any] | None = None
            for candidate in selected[target]["candidatePairs"]:
                with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
                    m3u_future = executor.submit(probe_stream, candidate["m3u"]["url"])
                    xtream_future = executor.submit(probe_stream, candidate["xtreamUrl"])
                    result = {"m3u": m3u_future.result(), "xtream": xtream_future.result()}
                attempts.append({"streamId": candidate["m3u"]["streamId"], **result})
                if working is None and result["m3u"].get("status") == "ok" and result["xtream"].get("status") == "ok":
                    working = candidate
                    direct[target] = result
            candidate_direct[target] = attempts
            if working is not None:
                selected[target] = working
            else:
                direct[target] = attempts[0] if attempts else {"m3u": {"status": "no-candidate"}, "xtream": {"status": "no-candidate"}}
        capacity = args.connection_cap if args.connection_cap is not None else int(live.get("IPTV_TEST_PROVIDER_MAX_CONNECTIONS", "3"))
        if capacity < 1:
            raise RuntimeError("the provider capacity must be positive")
        if args.stream_seconds < 1:
            raise RuntimeError("stream duration must be positive")
        reports = []
        build = not args.no_build
        kinds = ("M3U", "Xtream") if args.source == "both" else (args.source,)
        for kind in kinds:
            reports.append(run_stack(kind, selected, targets, xtream, capacity, temp_root, build, args.stream_seconds))
            build = False
        report: dict[str, Any] = {
            "targets": {
                target: {
                    "m3u": {"title": selected[target]["m3u"]["title"], "tvgId": selected[target]["m3u"]["tvgId"], "streamId": selected[target]["m3u"]["streamId"], "group": selected[target]["m3u"]["group"], "urlShape": safe_url_shape(selected[target]["m3u"]["url"])},
                    "xtream": {"name": selected[target]["xtream"].get("name", ""), "streamId": selected[target]["xtream"].get("stream_id"), "epgChannelId": selected[target]["xtream"].get("epg_channel_id", ""), "categoryId": selected[target]["xtream"].get("category_id", ""), "urlShape": safe_url_shape(selected[target]["xtreamUrl"])},
                    "sameStreamId": selected[target]["sameStreamId"],
                    "sameUrlShape": selected[target]["sameUrlShape"],
                    "sameExactUrl": selected[target]["sameExactUrl"],
                    "m3uUrlDigest": selected[target]["m3uUrlDigest"],
                    "xtreamUrlDigest": selected[target]["xtreamUrlDigest"],
                    "direct": direct[target],
                    "candidateDirect": candidate_direct[target],
                }
                for target in targets
            },
            "providerCapacity": capacity,
            "streamSeconds": args.stream_seconds,
            "localStacks": reports,
        }
        encoded = json.dumps(report, sort_keys=True)
        print(encoded, flush=True)
        if args.report_file:
            args.report_file.parent.mkdir(parents=True, exist_ok=True)
            args.report_file.write_text(encoded + "\n", encoding="utf-8")
        return 0
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"provider stream comparison failed: {error}", file=os.sys.stderr)
        return 1
    finally:
        shutil.rmtree(temp_root, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
