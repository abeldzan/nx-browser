#!/bin/sh
# Cargo runner: runs nxbrowser from inside a .app, since macOS only follows the light/dark/tinted theme for an
# icon from a bundle's Assets.car. Execs the binary directly so output stays in the terminal. Other binaries
# (tests) run as-is.
set -eu
bin=$1; shift
[ "$(basename "$bin")" = nxbrowser ] || exec "$bin" "$@"
root=$(cd "$(dirname "$0")/.." && pwd)
app=$(dirname "$bin")/NxBrowser.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$root/crates/app/Info.plist" "$app/Contents/Info.plist"
car=$app/Contents/Resources/Assets.car
if [ ! -f "$car" ] || [ -n "$(find "$root/crates/app/nxBrowser.icon" -newer "$car")" ]; then
    xcrun actool "$root/crates/app/nxBrowser.icon" --compile "$app/Contents/Resources" --app-icon nxBrowser \
        --target-device mac --platform macosx --minimum-deployment-target 14.0 \
        --output-partial-info-plist "$app/icon.plist" >/dev/null
    rm "$app/icon.plist"
fi
cp "$bin" "$app/Contents/MacOS/nxbrowser"
codesign --force --sign - "$app" 2>/dev/null
exec "$app/Contents/MacOS/nxbrowser" "$@"
