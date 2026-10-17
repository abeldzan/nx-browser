# List recipes.
default:
    @just --list

# Run the app (inside a dev .app wrapper, see scripts/run-bundled.sh).
run *args:
    cargo run -p nxbrowser -- {{args}}

# Build the release bundle: target/NxBrowser.app and target/NxBrowser.dmg.
bundle:
    scripts/bundle-macos.sh

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
