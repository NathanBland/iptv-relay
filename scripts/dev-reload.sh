#!/usr/bin/env bash
set -uo pipefail

watch_spec=$1
mode=$2
cargo_jobs=${IPTV_DEV_CARGO_JOBS:-2}
binary=/build/target/release/iptv-gateway
child_pid=
last_hash=

IFS=: read -r -a watch_paths <<< "$watch_spec"

stop_child() {
    if [[ -n "$child_pid" ]] && kill -0 "$child_pid" 2>/dev/null; then
        kill "$child_pid" 2>/dev/null || true
        for _ in {1..20}; do
            if ! kill -0 "$child_pid" 2>/dev/null; then
                break
            fi
            sleep 0.1
        done
        if kill -0 "$child_pid" 2>/dev/null; then
            kill -KILL "$child_pid" 2>/dev/null || true
        fi
        wait "$child_pid" 2>/dev/null || true
    fi
    child_pid=
}

stop_all() {
    stop_child
    exit 0
}

source_hash() {
    find "${watch_paths[@]}" -type f \( -name '*.rs' -o -name '*.toml' -o -name '*.sql' \) -print0 2>/dev/null \
        | sort -z \
        | xargs -0 -r stat -c '%Y:%s:%n' 2>/dev/null \
        | sha256sum \
        | cut -d' ' -f1
}

trap stop_all INT TERM

while true; do
    current_hash=$(source_hash)

    if [[ "$current_hash" != "$last_hash" ]]; then
        stop_child
        echo "[dev-reload] Build started."
        if cargo build --release --locked -j "$cargo_jobs" -p iptv-gateway --bin iptv-gateway; then
            "$binary" "$mode" &
            child_pid=$!
            echo "[dev-reload] Process started with PID $child_pid."
        else
            echo "[dev-reload] Build failed. Change a source file to retry."
        fi
        last_hash=$current_hash
    fi

    if [[ -n "$child_pid" ]] && ! kill -0 "$child_pid" 2>/dev/null; then
        wait "$child_pid" 2>/dev/null || true
        child_pid=
        echo "[dev-reload] Process stopped. Change a source file to retry."
    fi

    sleep 1
done
