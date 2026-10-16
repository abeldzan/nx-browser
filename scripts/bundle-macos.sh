#!/bin/sh
# Builds target/NxBrowser.app, ad-hoc signed, so macOS gives it a bundle ID (needed for system AutoFill).
set -eu
cd "$(dirname "$0")/.."

cargo build --release -p nxbrowser

app=target/NxBrowser.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/nxbrowser "$app/Contents/MacOS/nxbrowser"

cp crates/app/Info.plist "$app/Contents/Info.plist"

codesign --force --sign - "$app"
echo "Built $app"
