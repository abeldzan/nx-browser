#!/usr/bin/env bash
# Does what .github/workflows/release.yml does, from this machine: build the DMG, tag, publish the
# GitHub release and bump the Homebrew cask. The Release workflow is disabled while the tag is pushed,
# so the tag push doesn't start a CI run. GitHub drops events for disabled workflows; it doesn't queue them.
set -euo pipefail
cd "$(dirname "$0")/.."

# Same version checks as the workflow.
v=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' crates/app/Info.plist)
c=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ "$v" = "$c" ] || { echo "Info.plist version $v != Cargo.toml version $c (run scripts/bump-version.sh)" >&2; exit 1; }
tag="v$v"

# Preflight: fail before building or touching anything remote.
gh auth status >/dev/null
[ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "uncommitted changes" >&2; exit 1; }
[ "$(git branch --show-current)" = main ] || { echo "not on main" >&2; exit 1; }
git fetch -q origin main
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || { echo "HEAD != origin/main; push or pull first" >&2; exit 1; }
! git rev-parse -q --verify "refs/tags/$tag" >/dev/null || { echo "tag $tag exists locally" >&2; exit 1; }
[ -z "$(git ls-remote --tags origin "refs/tags/$tag")" ] || { echo "tag $tag exists on origin" >&2; exit 1; }

scripts/bundle-macos.sh

pushed_at=
reenable() {
    # shortcut: GitHub handles the push event asynchronously, so give it 60s before re-enabling; poll gh run list instead if a run ever slips through.
    if [ -n "$pushed_at" ]; then
        left=$((pushed_at + 60 - SECONDS))
        [ "$left" -le 0 ] || { echo "Waiting ${left}s before re-enabling the Release workflow"; sleep "$left"; }
    fi
    gh workflow enable release.yml
    gh run list --workflow release.yml -L1
}
gh workflow disable release.yml
trap reenable EXIT

git tag "$tag"
git push origin "$tag"
pushed_at=$SECONDS

gh release create "$tag" target/NxBrowser.dmg --title "$tag" --generate-notes --verify-tag

sha=$(shasum -a 256 target/NxBrowser.dmg | cut -d' ' -f1)
tap=$(mktemp -d)
git clone -q git@github.com:abeldzan/homebrew-tap.git "$tap"
sed -i '' -e "s/^  version \".*\"/  version \"$v\"/" -e "s/^  sha256 .*/  sha256 \"$sha\"/" "$tap/Casks/nxbrowser.rb"
git -C "$tap" commit -qam "nxbrowser $v"
git -C "$tap" push -q
rm -rf "$tap"

echo "Released $tag"
