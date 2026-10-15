#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
version="${1:?Missing semantic-release version}"
if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "Invalid release version: $version" >&2
  exit 1
fi

[[ -n "${OWLGOOD_UPDATER_PRIVATE_KEY:-}" ]] || { echo 'Missing release configuration: OWLGOOD_UPDATER_PRIVATE_KEY' >&2; exit 1; }
OWLGOOD_RELEASE_VERSION="$version" ./scripts/build-app.sh --universal
app="$PWD/target/release/OwlGood.app"
actual_version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")
[[ "$actual_version" == "$version" ]] || { echo 'Bundle version mismatch' >&2; exit 1; }
verify_universal() {
  local architectures
  architectures=$(lipo -archs "$1")
  [[ " $architectures " == *" arm64 "* && " $architectures " == *" x86_64 "* ]] || {
    echo 'Expected both arm64 and x86_64 in the application binary' >&2
    exit 1
  }
}
verify_universal "$app/Contents/MacOS/owlgood"
codesign --verify --deep --strict "$app"

staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT
mkdir -p release "$staging/dmg" "$staging/extracted"
base="OwlGood-${version}-macos-universal"
ditto "$app" "$staging/dmg/OwlGood.app"
ln -s /Applications "$staging/dmg/Applications"
hdiutil create -volname OwlGood -srcfolder "$staging/dmg" -ov -format UDZO "release/$base.dmg"
hdiutil verify "release/$base.dmg"
# Leave the DMG unsigned, matching Tauri: ad hoc signing the container can
# prevent Gatekeeper from opening it. The enclosed application remains signed.
# https://github.com/tauri-apps/tauri/issues/12288
tar -czf "release/$base.app.tar.gz" -C target/release OwlGood.app
tar -xzf "release/$base.app.tar.gz" -C "$staging/extracted"
codesign --verify --deep --strict "$staging/extracted/OwlGood.app"
verify_universal "$staging/extracted/OwlGood.app/Contents/MacOS/owlgood"
# Private material is passed through stdin, never command arguments or logs.
printf '%s' "$OWLGOOD_UPDATER_PRIVATE_KEY" | target/sparkle-sdk/bin/sign_update --ed-key-file - -p "release/$base.app.tar.gz" > "release/$base.app.tar.gz.sig"
node scripts/updater-release.mjs artifacts "$version"
printf '%s' "$OWLGOOD_UPDATER_PRIVATE_KEY" | target/sparkle-sdk/bin/sign_update --ed-key-file - release/appcast.xml
printf '%s' "$OWLGOOD_UPDATER_PRIVATE_KEY" | target/sparkle-sdk/bin/sign_update --ed-key-file - --verify release/appcast.xml
node scripts/updater-release.mjs checksums "$version"
