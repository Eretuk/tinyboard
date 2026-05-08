#!/bin/sh
set -e

CONFIG=/data/tinyboard/config.yaml
BOARD=/data/tinyboard/board.yaml
DATA_DIR=/data/tinyboard

# Verify the data directory is writable.
# If not, the host directory likely has wrong ownership.
# Fix with: sudo chown -R 10001:10001 ./data
if [ ! -w "$DATA_DIR" ]; then
    echo "ERROR: $DATA_DIR is not writable by UID $(id -u)."
    echo "Fix ownership on the host: sudo chown -R 10001:10001 ./data"
    exit 1
fi

# Copy defaults into the volume on first run (don't overwrite existing files)
if [ ! -f "$CONFIG" ]; then
    echo "Creating default config.yaml"
    cp /app/defaults/config.yaml "$CONFIG"
fi

if [ ! -f "$BOARD" ]; then
    echo "Creating default board.yaml"
    cp /app/defaults/board.yaml "$BOARD"
fi

exec tinyboard "$@"
