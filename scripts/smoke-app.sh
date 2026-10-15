#!/bin/sh
# Exercise real native windows using disposable preferences.
set -eu
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
APP="$ROOT/target/release/isDown.app"
if [ ! -x "$APP/Contents/MacOS/isdown" ]; then
    echo 'Run ./scripts/build-app.sh first.' >&2
    exit 1
fi
SMOKE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/isdown-smoke.XXXXXX")
trap 'rm -rf "$SMOKE_DIR"' EXIT HUP INT TERM
ISDOWN_CONFIG_PATH="$SMOKE_DIR/config.json" ISDOWN_SMOKE_TEST=1 \
    "$APP/Contents/MacOS/isdown"
