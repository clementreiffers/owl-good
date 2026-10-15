#!/usr/bin/env bash
# Pinned SDK and tools; no mutable latest-release download in the build.
set -euo pipefail
cd "$(dirname "$0")/.."
version=2.10.0
checksum=c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c
mkdir -p target/sparkle-sdk
archive="target/sparkle-sdk/Sparkle-$version.tar.xz"
if [[ ! -f "$archive" ]]; then
  curl --fail --location --proto '=https' --proto-redir '=https' --retry 3 \
    "https://github.com/sparkle-project/Sparkle/releases/download/$version/Sparkle-$version.tar.xz" -o "$archive"
fi
printf '%s  %s\n' "$checksum" "$archive" | shasum -a 256 --check --status
tar -xf "$archive" -C target/sparkle-sdk
