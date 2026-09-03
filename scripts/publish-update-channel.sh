#!/usr/bin/env bash
#
# Publish the AppImage and its .zsync to the fixed `linux-latest` tag on the
# GitHub mirror — the URL every installed copy checks for updates.
#
# This exists because the update URL has to be one that never moves.
# `releases/latest` does move: it follows whatever release is newest, and the
# Gitea-to-GitHub backfill creates one GitHub release per Gitea tag, including
# the `-win` and `-mac` tags that carry no AppImage. Pointing a million
# installed copies at a URL that can resolve to a release with no AppImage in
# it is a failure that shows up on users' machines and nowhere else.
#
# So this tag holds exactly two files, replaced in place on every release.
# The versioned per-release artifacts are published separately and are what a
# human downloads; this is what the updater reads.
#
# It writes to GitHub rather than Gitea because that mirror is where updates
# are pulled from. Needs GH_PAT with contents write on the mirror.
#
# Usage: GH_PAT=... publish-update-channel.sh <directory holding the artifacts>

set -euo pipefail

REPO="shadowdao/triple-c"
TAG="linux-latest"
API="https://api.github.com/repos/$REPO"
ASSETS=("Triple-C_x86_64.AppImage" "Triple-C_x86_64.AppImage.zsync")

: "${GH_PAT:?GH_PAT is required to publish the update channel}"
dir="${1:?usage: publish-update-channel.sh <artifacts directory>}"
cd "$dir"

for asset in "${ASSETS[@]}"; do
  [ -e "$asset" ] || { echo "Missing $asset in $dir" >&2; exit 1; }
done

gh() { curl -sf -H "Authorization: Bearer $GH_PAT" -H "Accept: application/vnd.github+json" "$@"; }

echo "==> Looking for the $TAG release"
release="$(gh "$API/releases/tags/$TAG" 2>/dev/null || true)"
release_id="$(printf '%s' "$release" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("id",""))' 2>/dev/null || true)"

if [ -z "$release_id" ]; then
  echo "==> Creating it"
  # Not a prerelease, but deliberately not the "latest" release either: this
  # tag is a channel, and it must never displace the versioned release a
  # person lands on from the releases page.
  release="$(gh -X POST "$API/releases" -d "$(python3 -c '
import json
print(json.dumps({
    "tag_name": "'"$TAG"'",
    "name": "Linux update channel",
    "body": "Rolling AppImage build that Triple-C’s in-app updater reads. "
            "The two files here are replaced on every release; for a specific "
            "version, use the versioned releases instead.",
    "draft": False,
    "prerelease": False,
    "make_latest": "false",
}))')")"
  release_id="$(printf '%s' "$release" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])')"
fi

echo "==> Removing superseded assets from release $release_id"
printf '%s' "$release" | python3 -c '
import sys, json
keep = set(sys.argv[1:])
for a in json.load(sys.stdin).get("assets", []):
    if a["name"] in keep:
        print(a["id"])
' "${ASSETS[@]}" | while read -r asset_id; do
  [ -n "$asset_id" ] || continue
  gh -X DELETE "$API/releases/assets/$asset_id" >/dev/null || true
done

for asset in "${ASSETS[@]}"; do
  echo "==> Uploading $asset ($(du -h "$asset" | cut -f1))"
  curl -sf -X POST \
    -H "Authorization: Bearer $GH_PAT" \
    -H "Content-Type: application/octet-stream" \
    --data-binary "@$asset" \
    "https://uploads.github.com/repos/$REPO/releases/$release_id/assets?name=$asset" >/dev/null
done

# The updater is only as good as this URL, and a silent failure here means
# every installed copy quietly stops updating. Confirm both are actually
# fetchable at the address the AppImage was built to check.
echo "==> Verifying the published URLs"
for asset in "${ASSETS[@]}"; do
  url="https://github.com/$REPO/releases/download/$TAG/$asset"
  code="$(curl -s -o /dev/null -w '%{http_code}' -L "$url")"
  [ "$code" = "200" ] || { echo "FAILED: $url returned $code" >&2; exit 1; }
  echo "    $code  $url"
done

echo "OK: $TAG updated."
