#!/bin/bash
# Poll-based reload script for Docker dev environments on macOS.
# Docker bind mounts on macOS do not propagate inotify events,
# so we poll file modification times instead.
set -e

WATCH_PATHS="$1"
shift
CMD="$@"

LAST_HASH=""

while true; do
    # Compute a hash of all mtimes in the watched paths
    CURRENT_HASH=$(find $WATCH_PATHS -type f -name '*.rs' -o -name '*.toml' -o -name '*.sql' 2>/dev/null | xargs stat -c '%Y %n' 2>/dev/null | sort | md5sum | cut -d' ' -f1)

    if [ "$CURRENT_HASH" != "$LAST_HASH" ]; then
        if [ -n "$LAST_HASH" ]; then
            echo "[dev-reload] Change detected, rebuilding..."
            # Kill the previous process if it's running
            if [ -n "$CHILD_PID" ]; then
                kill "$CHILD_PID" 2>/dev/null || true
                wait "$CHILD_PID" 2>/dev/null || true
            fi
            # Rebuild and run
            $CMD &
            CHILD_PID=$!
            echo "[dev-reload] Started PID $CHILD_PID"
        else
            # First run
            echo "[dev-reload] Initial build..."
            $CMD &
            CHILD_PID=$!
            echo "[dev-reload] Started PID $CHILD_PID"
        fi
        LAST_HASH="$CURRENT_HASH"
    fi

    # Check if the child process exited
    if [ -n "$CHILD_PID" ] && ! kill -0 "$CHILD_PID" 2>/dev/null; then
        echo "[dev-reload] Process exited. Waiting for changes..."
        wait "$CHILD_PID" 2>/dev/null || true
        CHILD_PID=""
    fi

    sleep 1
done
