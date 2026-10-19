# List recipes.
default:
    @just --list

# Run the app (inside a dev .app wrapper, see scripts/run-bundled.sh).
run *args:
    cargo run -p nxbrowser -- {{args}}

# Build the release bundle: target/NxBrowser.app and target/NxBrowser.dmg.
bundle:
    scripts/bundle-macos.sh

# Bump the version in Cargo.toml, Info.plist and Cargo.lock, e.g. `just bump 0.1.3`.
bump version:
    scripts/bump-version.sh {{version}}

# Tag the current version (v<Cargo.toml version>) and push the tag, which starts the Release workflow.
tag:
    #!/usr/bin/env bash
    set -euo pipefail
    v=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
    p=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' crates/app/Info.plist)
    [ "$v" = "$p" ] || { echo "Cargo.toml $v != Info.plist $p (run just bump)" >&2; exit 1; }
    git tag "v$v"
    git push origin "v$v"

# Release from this machine (build, tag, GitHub release, cask) without running the Release workflow.
release-local:
    scripts/release-local.sh

# Build the release bundle and open it.
open: bundle
    open target/NxBrowser.app

# Unit tests.
test:
    cargo test -p browser-core -p browser-extensions -p browser-ui

# End-to-end check: profiles, JS, resizing, example extension; exits when done.
self-test:
    cargo run -p nxbrowser -- --self-test

# Everything to run before a PR.
check: lint test self-test

lint:
    cargo clippy --workspace

# Open a link as another app would, to try the default-browser handler.
link url="https://example.com":
    open "{{url}}"

# Regenerate the built-in ad/tracker block list.
blocklist:
    scripts/update-blocklist.sh
