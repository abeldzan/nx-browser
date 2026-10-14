#!/bin/sh
# Builds target/NxBrowser.app, ad-hoc signed, so macOS gives it a bundle ID (needed for system AutoFill).
set -eu
cd "$(dirname "$0")/.."

cargo build --release -p nxbrowser

app=target/NxBrowser.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/nxbrowser "$app/Contents/MacOS/nxbrowser"

cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>dev.nxbrowser.app</string>
    <key>CFBundleName</key><string>NxBrowser</string>
    <key>CFBundleExecutable</key><string>nxbrowser</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign --force --sign - "$app"
echo "Built $app"
