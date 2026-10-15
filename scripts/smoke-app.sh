#!/bin/sh
# Exercise real native windows using disposable preferences.
set -eu
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
APP="$ROOT/target/release/OwlGood.app"
if [ ! -x "$APP/Contents/MacOS/owlgood" ]; then
    echo 'Run ./scripts/build-app.sh first.' >&2
    exit 1
fi
SMOKE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/owlgood-smoke.XXXXXX")
trap 'rm -rf "$SMOKE_DIR"' EXIT HUP INT TERM
OWLGOOD_CONFIG_PATH="$SMOKE_DIR/config.json" OWLGOOD_SMOKE_TEST=1 OWLGOOD_SMOKE_AUTOSAVE=1 \
    "$APP/Contents/MacOS/owlgood"
