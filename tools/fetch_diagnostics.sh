#!/usr/bin/env bash
# Maintainer tool: downloads and unpacks a diagnostics bundle by its report code
# (docs/diagnostics.md). Needs `wrangler` logged in to the Cloudflare account (cd server/worker).
#
#   tools/fetch_diagnostics.sh W2R-7K3F-92QX     # fetch into diagnostics-fetched/W2R-7K3F-92QX/
#   tools/fetch_diagnostics.sh --list [N]         # the N newest uploads (default 20)
#
# WRANGLER overrides the command (default: npx wrangler), OUT the output folder.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
wrangler=${WRANGLER:-npx wrangler}
out_root=${OUT:-$here/diagnostics-fetched}
cd "$here/server/worker"

query() {
  # wrangler prints [{"results":[...],...}]; print one JSON object per row.
  $wrangler d1 execute w2r-stats --remote --json --command "$1" |
    node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{for(const r of JSON.parse(s)[0].results)console.log(JSON.stringify(r))})'
}

case "${1:-}" in
  ""|-h|--help)
    sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
    exit 2 ;;
  --list)
    n=${2:-20}
    [[ $n =~ ^[0-9]{1,3}$ ]] || { echo "N must be a number" >&2; exit 2; }
    query "SELECT code, datetime(received,'unixepoch') AS received, size, substr(key,1,8) AS key FROM diagnostics ORDER BY received DESC LIMIT $n"
    exit 0 ;;
esac

code=$(printf '%s' "$1" | tr a-z A-Z)
# The pattern is also what keeps the code safe to put in the query below.
[[ $code =~ ^W2R-[2-9A-HJ-NP-Z]{4}-[2-9A-HJ-NP-Z]{4}$ ]] || { echo "not a report code: $1" >&2; exit 2; }

sha=$(query "SELECT sha FROM diagnostics WHERE code = '$code'" |
  node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const l=s.trim().split("\n")[0];if(l)console.log(JSON.parse(l).sha)})')
[[ $sha =~ ^[0-9a-f]{64}$ ]] || { echo "no bundle with code $code (expired after 30 days, or never uploaded)" >&2; exit 1; }

dest="$out_root/$code"
[[ ! -e $dest ]] || { echo "$dest already exists; remove it first" >&2; exit 1; }
mkdir -p "$dest"
trap 'rm -rf "$dest"' ERR
$wrangler r2 object get "w2r-replays/diagnostics/$sha.zip" --remote --file "$dest/bundle.zip"
unzip -q "$dest/bundle.zip" -d "$dest/files"
echo "unpacked to $dest/files"
ls -l "$dest/files" "$dest/files/logs" 2>/dev/null | sed 's/^/  /'
