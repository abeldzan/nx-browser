#!/usr/bin/env bash
# Bump the release version everywhere the release workflow checks it:
# Cargo.toml (workspace), crates/app/Info.plist (CFBundleShortVersionString),
# Cargo.lock (regenerated). Tag must then match: v<x.y.z>.
set -euo pipefail
cd "$(dirname "$0")/.."

v="${1:?usage: scripts/bump-version.sh <x.y.z>}"
[[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "bad version: $v (want x.y.z)" >&2; exit 1; }

sed -i '' "s/^version = \".*\"/version = \"$v\"/" Cargo.toml
sed -i '' -E 's#(<key>CFBundleShortVersionString</key><string>)[^<]*(</string>)#\1'"$v"'\2#' crates/app/Info.plist
cargo update -w

# Same asserts the release workflow runs; fail before committing if they diverge.
[ "$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' crates/app/Info.plist)" = "$v" ]
[ "$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)" = "$v" ]

echo "bumped to $v — commit, then tag v$v and push the tag to trigger the release"
