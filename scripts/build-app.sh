#!/bin/sh
# Build a native, locally signed macOS application bundle.
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"
if [ "$(uname -s)" != Darwin ]; then
    echo 'isDown requires macOS to build its AppKit application.' >&2
    exit 1
fi

export MACOSX_DEPLOYMENT_TARGET=13.0
if [ "${1:-}" = '--universal' ]; then
    cargo build --release --locked --target aarch64-apple-darwin
    cargo build --release --locked --target x86_64-apple-darwin
    mkdir -p "$ROOT/target/universal-apple-darwin/release"
    lipo -create "$ROOT/target/aarch64-apple-darwin/release/isdown" \
        "$ROOT/target/x86_64-apple-darwin/release/isdown" \
        -output "$ROOT/target/universal-apple-darwin/release/isdown"
    BINARY="$ROOT/target/universal-apple-darwin/release/isdown"
else
    cargo build --release --locked
    BINARY="$ROOT/target/release/isdown"
fi
APP="$ROOT/target/release/isDown.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/isdown"
cp "$ROOT/resources/Info.plist" "$APP/Contents/Info.plist"
if [ -n "${ISDOWN_RELEASE_VERSION:-}" ]; then
    /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $ISDOWN_RELEASE_VERSION" "$APP/Contents/Info.plist"
    /usr/libexec/PlistBuddy -c "Set :CFBundleVersion $ISDOWN_RELEASE_VERSION" "$APP/Contents/Info.plist"
fi
printf 'APPL????' > "$APP/Contents/PkgInfo"
plutil -lint "$APP/Contents/Info.plist"
codesign --force --sign - --identifier com.isdown.app "$APP"
codesign --verify --strict "$APP"
echo "$APP"
