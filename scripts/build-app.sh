#!/bin/sh
# Build a native, locally signed macOS application bundle.
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"
if [ "$(uname -s)" != Darwin ]; then
    echo 'OwlGood requires macOS to build its AppKit application.' >&2
    exit 1
fi

export MACOSX_DEPLOYMENT_TARGET=13.0
if [ "${1:-}" = '--universal' ]; then
    cargo build --release --locked --target aarch64-apple-darwin
    cargo build --release --locked --target x86_64-apple-darwin
    mkdir -p "$ROOT/target/universal-apple-darwin/release"
    lipo -create "$ROOT/target/aarch64-apple-darwin/release/owlgood" \
        "$ROOT/target/x86_64-apple-darwin/release/owlgood" \
        -output "$ROOT/target/universal-apple-darwin/release/owlgood"
    BINARY="$ROOT/target/universal-apple-darwin/release/owlgood"
else
    cargo build --release --locked
    BINARY="$ROOT/target/release/owlgood"
fi
APP="$ROOT/target/release/OwlGood.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/owlgood"
cp "$ROOT/resources/Info.plist" "$APP/Contents/Info.plist"
if [ -n "${OWLGOOD_RELEASE_VERSION:-}" ]; then
    /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $OWLGOOD_RELEASE_VERSION" "$APP/Contents/Info.plist"
    /usr/libexec/PlistBuddy -c "Set :CFBundleVersion $OWLGOOD_RELEASE_VERSION" "$APP/Contents/Info.plist"
fi
printf 'APPL????' > "$APP/Contents/PkgInfo"
plutil -lint "$APP/Contents/Info.plist"
codesign --force --sign - --identifier com.owlgood.app "$APP"
codesign --verify --strict "$APP"
echo "$APP"
