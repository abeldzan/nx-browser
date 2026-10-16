#!/bin/sh
# Regenerates the built-in ad/tracker block list from Peter Lowe's ad server list (https://pgl.yoyo.org/adservers/)
# as WebKit content rules: one third-party block rule per host, so visiting a listed site directly still works.
set -eu
cd "$(dirname "$0")/.."

out=crates/browser-engine-macos/src/blocklist.json
curl -fsS "https://pgl.yoyo.org/adservers/serverlist.php?hostformat=nohtml&showintro=0&mimetype=plaintext" |
  grep -E '^[A-Za-z0-9.-]+$' |
  sed 's/\./\\\\./g' |
  awk 'BEGIN { printf "[" }
       { printf "%s{\"trigger\":{\"url-filter\":\"^[^:]+://([^/]+\\\\.)?%s[:/]\",\"load-type\":[\"third-party\"]},\"action\":{\"type\":\"block\"}}\n", (NR > 1 ? "," : ""), $0 }
       END { print "]" }' > "$out"
echo "$(grep -c url-filter "$out") rules -> $out"
