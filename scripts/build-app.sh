#!/bin/sh
# Build a native, locally signed macOS application bundle.
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"
if [ "$(uname -s)" != Darwin ]; then
    echo 'OwlGood requires macOS to build its AppKit application.' >&2
    exit 1
fi

swift -module-cache-path "$ROOT/target/swift-module-cache" \
    "$ROOT/scripts/build-icon.swift" "$ROOT/resources/owlgood-app.svg" "$ROOT/target/app-icon"
bash "$ROOT/scripts/fetch-sparkle.sh"
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
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
ditto "$ROOT/target/sparkle-sdk/Sparkle.framework" "$APP/Contents/Frameworks/Sparkle.framework"
cp "$ROOT/target/sparkle-sdk/LICENSE" "$APP/Contents/Resources/Sparkle-LICENSE"
cp "$BINARY" "$APP/Contents/MacOS/owlgood"
cp "$ROOT/resources/Info.plist" "$APP/Contents/Info.plist"
cp "$ROOT/target/app-icon/OwlGood.icns" "$APP/Contents/Resources/OwlGood.icns"
if [ -n "${OWLGOOD_RELEASE_VERSION:-}" ]; then
    /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $OWLGOOD_RELEASE_VERSION" "$APP/Contents/Info.plist"
    /usr/libexec/PlistBuddy -c "Set :CFBundleVersion $OWLGOOD_RELEASE_VERSION" "$APP/Contents/Info.plist"
fi
public_key=$(cat "$ROOT/resources/updater-public-key.txt")
/usr/libexec/PlistBuddy -c "Add :SUPublicEDKey string $public_key" "$APP/Contents/Info.plist"
# Sign nested Sparkle helpers before the containing framework and host app.
framework="$APP/Contents/Frameworks/Sparkle.framework/Versions/B"
codesign --force --sign - "$framework/XPCServices/Downloader.xpc"
codesign --force --sign - "$framework/XPCServices/Installer.xpc"
codesign --force --sign - "$framework/Updater.app"
codesign --force --sign - "$framework/Autoupdate"
codesign --force --sign - "$APP/Contents/Frameworks/Sparkle.framework"
printf 'APPL????' > "$APP/Contents/PkgInfo"
plutil -lint "$APP/Contents/Info.plist"
codesign --force --sign - --identifier com.owlgood.app "$APP"
codesign --verify --deep --strict "$APP"
echo "$APP"
