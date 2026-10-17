#!/bin/sh
# Builds target/NxBrowser.app, ad-hoc signed, so macOS gives it a bundle ID (needed for system AutoFill),
# and target/NxBrowser.dmg holding it beside an Applications link for drag-to-install.
set -eu
cd "$(dirname "$0")/.."

cargo build --release -p nxbrowser

app=target/NxBrowser.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/nxbrowser "$app/Contents/MacOS/nxbrowser"

cp crates/app/Info.plist "$app/Contents/Info.plist"

# Compiles the Icon Composer icon into Assets.car (Liquid Glass) plus an .icns fallback.
mkdir -p "$app/Contents/Resources"
xcrun actool crates/app/nxBrowser.icon --compile "$app/Contents/Resources" --app-icon nxBrowser \
    --target-device mac --platform macosx --minimum-deployment-target 14.0 \
    --output-partial-info-plist "$app/icon.plist" >/dev/null
rm "$app/icon.plist"

codesign --force --sign - "$app"
echo "Built $app"

dmg=target/NxBrowser.dmg
stage=target/dmg
rm -rf "$stage"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -volname NxBrowser -srcfolder "$stage" -ov -format UDZO "$dmg" >/dev/null
rm -rf "$stage"
echo "Built $dmg"
