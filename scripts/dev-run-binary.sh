#!/usr/bin/env bash
set -uo pipefail

binary=$1
mode=$2
health_file=${IPTV_DEV_HEALTH_FILE:-/tmp/iptv-worker-active}
child_pid=
last_signature=

stop_child() {
    rm -f "$health_file"
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

trap stop_all INT TERM

while true; do
    if [[ -x "$binary" ]]; then
        signature=$(stat -c '%Y:%s' "$binary")
        if [[ "$signature" != "$last_signature" ]]; then
            stop_child
            "$binary" "$mode" &
            child_pid=$!
            : > "$health_file"
            last_signature=$signature
            echo "[dev-run-binary] Process started with PID $child_pid."
        fi
    fi

    if [[ -n "$child_pid" ]] && ! kill -0 "$child_pid" 2>/dev/null; then
        wait "$child_pid" 2>/dev/null || true
        child_pid=
        rm -f "$health_file"
        echo "[dev-run-binary] Process stopped. Waiting for a new binary."
    fi

    sleep 1
done
