#!/bin/sh
# Development tool: saves MacCleaner screenshots rendered by the app itself (no screen capture).
# Usage: scripts/snapshot.sh <output-dir> [section[:scan]...]   (e.g. largeFiles:scan devJunk)
# `:scan` runs the section's analysis before capturing it.
# The app must be running with:  open --env MACCLEANER_SNAPSHOT_DIR=<output-dir> build/MacCleaner.app
set -eu
DIR="$1"; shift
mkdir -p "$DIR"
for SECTION in "${@:-}"; do
    osascript -l JavaScript -e "ObjC.import('Foundation');
        \$.NSDistributedNotificationCenter.defaultCenter.postNotificationNameObjectUserInfoDeliverImmediately(
            'com.maccleaner.debug.snapshot', '$SECTION', \$(), true)" >/dev/null
    NAME="${SECTION%%:*}"
    # Wait for the image to appear (analyses can take a while).
    i=0; while [ ! -f "$DIR/${NAME:-ventana}.png" ] && [ $i -lt 120 ]; do sleep 1; i=$((i+1)); done
    echo "$DIR/${NAME:-ventana}.png"
done
