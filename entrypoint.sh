#!/bin/sh
set -e

CONFIG=/data/tinyboard/config.yaml
BOARD=/data/tinyboard/board.yaml

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
