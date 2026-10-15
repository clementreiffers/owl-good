#!/bin/sh
# Build a native, locally signed macOS application bundle.
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"
if [ "$(uname -s)" != Darwin ]; then
    echo 'isDown requires macOS to build its AppKit application.' >&2
    exit 1
fi

cargo build --release --locked
APP="$ROOT/target/release/isDown.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/release/isdown" "$APP/Contents/MacOS/isdown"
cp "$ROOT/resources/Info.plist" "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
plutil -lint "$APP/Contents/Info.plist"
codesign --force --sign - --identifier com.isdown.app "$APP"
codesign --verify --strict "$APP"
echo "$APP"
