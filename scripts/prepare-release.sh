#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
version="${1:?Missing semantic-release version}"
if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "Invalid release version: $version" >&2
  exit 1
fi

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
tar -czf "release/$base.app.tar.gz" -C target/release OwlGood.app
tar -xzf "release/$base.app.tar.gz" -C "$staging/extracted"
codesign --verify --deep --strict "$staging/extracted/OwlGood.app"
verify_universal "$staging/extracted/OwlGood.app/Contents/MacOS/owlgood"
(cd release && shasum -a 256 "$base.dmg" "$base.app.tar.gz" > SHA256SUMS.txt)
