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
# **The tag has to exist in Gitea, not just on GitHub, and that is the whole
# reason this script touches Gitea at all.** Gitea push-mirrors this repo to
# GitHub, and a mirror push deletes remote refs that have no local counterpart.
# A tag created only by GitHub's release API therefore survives until the next
# mirror run and then vanishes — which is exactly what happened to 0.4.20 and
# 0.4.21: the release was created and both URLs verified 200 at 00:38, and the
# 13:04 mirror deleted the tag, leaving every installed copy checking a 404.
# Versioned tags never had this problem because `create-tag` creates them in
# Gitea first. So does this one, now, and before the GitHub release rather than
# after, so there is no window where the two disagree.
#
# Note what this means for verification: publishing correctly is not evidence
# the channel still works hours later. The Gitea tag is what makes it durable,
# so its absence is treated as a failure rather than a warning.
#
# Usage: GH_PAT=... GITEA_TOKEN=... GITEA_SHA=... publish-update-channel.sh <dir>

set -euo pipefail

REPO="shadowdao/triple-c"
TAG="linux-latest"
API="https://api.github.com/repos/$REPO"
ASSETS=("Triple-C_x86_64.AppImage" "Triple-C_x86_64.AppImage.zsync")

GITEA_API="${GITEA_API:-https://repo.anhonesthost.net/api/v1}"
GITEA_REPO="${GITEA_REPO:-CyberCoveLLC/Triple-C}"

: "${GH_PAT:?GH_PAT is required to publish the update channel}"
: "${GITEA_TOKEN:?GITEA_TOKEN is required to anchor the $TAG tag against the mirror}"
: "${GITEA_SHA:?GITEA_SHA is required to point the $TAG tag at this build}"
dir="${1:?usage: publish-update-channel.sh <artifacts directory>}"
cd "$dir"

for asset in "${ASSETS[@]}"; do
  [ -e "$asset" ] || { echo "Missing $asset in $dir" >&2; exit 1; }
done

gh() { curl -sf -H "Authorization: Bearer $GH_PAT" -H "Accept: application/vnd.github+json" "$@"; }
tea() { curl -sf -H "Authorization: token $GITEA_TOKEN" -H "Content-Type: application/json" "$@"; }
# Status, not a boolean. `curl -sf` fails identically for "404, the tag is
# genuinely absent" and "503, Gitea is briefly unreachable", and treating the
# second as the first means POSTing over a tag that already exists, taking a
# 409, and aborting the last step of build-linux — which `create-tag` and
# `sync-to-github` both depend on. A transient blip would cost the release, not
# just the channel update. Same `case`-on-code idiom as `Upload to Gitea
# release` two steps above in the workflow. A refused connection reports 000
# and lands in the catch-all.
tea_code() { curl -s -o /dev/null -w '%{http_code}' -H "Authorization: token $GITEA_TOKEN" "$@"; }

# Anchor the tag in Gitea — see the header. **Created if absent, never moved.**
#
# An earlier version deleted and recreated it so the tag would name the current
# build. That was worse than useless: nothing about the channel depends on
# which commit the tag points at — the update string resolves the tag by *name*
# and the assets hang off the release object — while a DELETE followed by a
# failed POST destroys a working anchor and leaves a window in which a mirror
# run prunes GitHub's copy. A transient Gitea error would have converted a
# healthy channel into a dead one, which is strictly worse than this step not
# existing. Gitea's POST /tags has no force semantics, so the DELETE was only
# ever there to get around a 409; asking first removes the need.
echo "==> Anchoring the $TAG tag in Gitea"
anchor_probe="$(tea_code "$GITEA_API/repos/$GITEA_REPO/tags/$TAG")"
case "$anchor_probe" in
  200)
    echo "    already anchored — left alone"
    ;;
  404)
    echo "    creating it at ${GITEA_SHA:0:9}"
    tea -X POST "$GITEA_API/repos/$GITEA_REPO/tags" \
      -d "{\"tag_name\": \"$TAG\", \"target\": \"$GITEA_SHA\", \"message\": \"Rolling Linux update channel\"}" \
      >/dev/null
    ;;
  *)
    echo "FAILED: Gitea answered $anchor_probe asking whether the $TAG tag exists." >&2
    echo "        Refusing to guess — creating it blindly would 409 over an" >&2
    echo "        existing tag and abort the release." >&2
    exit 1
    ;;
esac

# Not best-effort. Without this tag the mirror removes GitHub's and the
# channel dies silently somewhere between now and four hours from now. Reported
# by code, so "Gitea was unreachable" cannot masquerade as "the tag is gone".
anchor_code="$(tea_code "$GITEA_API/repos/$GITEA_REPO/tags/$TAG")"
[ "$anchor_code" = "200" ] || {
  echo "FAILED: the $TAG tag is not readable in Gitea (HTTP $anchor_code);" >&2
  echo "        without it the mirror would delete GitHub's copy." >&2
  exit 1
}

# Look through the authenticated list rather than /releases/tags/, which never
# returns drafts. That matters here specifically: GitHub demotes a published
# release to a draft when its tag is deleted, which is the state every mirror
# run left behind, so the by-tag lookup reports "absent" while orphaned drafts
# sit there holding 86 MB each. Reuse the newest and delete the rest, or they
# accumulate one per release forever.
echo "==> Looking for the $TAG release (drafts included)"
all_releases="$(gh "$API/releases?per_page=100")"
mapfile -t existing < <(printf '%s' "$all_releases" | python3 -c '
import sys, json
tag = sys.argv[1]
rs = [r for r in json.load(sys.stdin) if r.get("tag_name") == tag]
rs.sort(key=lambda r: r.get("created_at",""), reverse=True)
for r in rs:
    print(r["id"])
' "$TAG")

release_id="${existing[0]:-}"

for stale in "${existing[@]:1}"; do
  echo "    deleting orphaned duplicate release $stale"
  gh -X DELETE "$API/releases/$stale" >/dev/null || true
done

if [ -n "$release_id" ]; then
  # A draft has no tag and serves no download URL, so it has to be republished.
  echo "    reusing release $release_id"
  # `make_latest` is not optional here even though this release already exists.
  # Publishing a draft is a publish transition, where the API's documented
  # default is `true` — so omitting it would quietly promote this channel to
  # the repository's "Latest release" and bury the versioned release a person
  # actually wants from the releases page.
  #
  # `tag_name` is re-sent deliberately, and must be: the API removes the tag
  # when a PATCH omits it. Given this whole change exists because a tag
  # disappeared, that is an expensive line to tidy away.
  gh -X PATCH "$API/releases/$release_id" \
    -d "{\"tag_name\": \"$TAG\", \"draft\": false, \"make_latest\": \"false\"}" >/dev/null
  release="$(gh "$API/releases/$release_id")"
fi

if [ -z "$release_id" ]; then
  echo "==> Creating it"
  # Not a prerelease, but deliberately not the "latest" release either: this
  # tag is a channel, and it must never displace the versioned release a
  # person lands on from the releases page.
  body_json="$(python3 -c '
import json
print(json.dumps({
    "tag_name": "'"$TAG"'",
    "name": "Linux update channel",
    "body": "Rolling AppImage build that Triple-C\u2019s in-app updater reads. "
            "The two files here are replaced on every release; for a specific "
            "version, use the versioned releases instead.",
    "draft": False,
    "prerelease": False,
    "make_latest": "false",
}))')"

  # `already_exists` is a benign, recoverable answer, not a reason to abort the
  # last step of build-linux and lose the release with it. It means a release
  # for this tag exists but the listing above did not show it — a draft that has
  # sunk past the first page, since a draft's created_at is frozen while newer
  # releases push it down. Re-ask by tag and carry on.
  create_body="$(mktemp)"
  create_code="$(curl -s -o "$create_body" -w '%{http_code}' \
    -H "Authorization: Bearer $GH_PAT" -H "Accept: application/vnd.github+json" \
    -X POST "$API/releases" -d "$body_json")"

  case "$create_code" in
    201)
      release="$(cat "$create_body")"
      ;;
    422)
      if grep -q "already_exists" "$create_body"; then
        echo "    a release for $TAG already exists but was not listed — reusing it"
        release="$(gh "$API/releases/tags/$TAG")"
      else
        echo "FAILED: GitHub rejected the release (422):" >&2
        cat "$create_body" >&2
        rm -f "$create_body"
        exit 1
      fi
      ;;
    *)
      echo "FAILED: creating the $TAG release returned $create_code:" >&2
      cat "$create_body" >&2
      rm -f "$create_body"
      exit 1
      ;;
  esac
  rm -f "$create_body"

  release_id="$(printf '%s' "$release" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])')"
fi

# One asset at a time, delete immediately followed by upload. Deleting both up
# front leaves the channel holding a fresh AppImage and no .zsync if the second
# upload fails, and a client that cannot fetch the .zsync simply stops updating
# — no error anyone here would see.
asset_ids="$(printf '%s' "$release" | python3 -c '
import sys, json
keep = set(sys.argv[1:])
out = {}
for a in json.load(sys.stdin).get("assets", []):
    if a["name"] in keep:
        out[a["name"]] = a["id"]
print(json.dumps(out))
' "${ASSETS[@]}")"

# --retry/--max-time/--http1.1 for the reason the Gitea upload steps in this
# repo carry them: real mid-stream failures on large assets (curl 92 and 28).
for asset in "${ASSETS[@]}"; do
  stale_id="$(printf '%s' "$asset_ids" | python3 -c 'import sys,json;print(json.load(sys.stdin).get(sys.argv[1],""))' "$asset")"
  if [ -n "$stale_id" ]; then
    echo "==> Replacing $asset (dropping superseded asset $stale_id)"
    gh -X DELETE "$API/releases/assets/$stale_id" >/dev/null || true
  fi
  echo "==> Uploading $asset ($(du -h "$asset" | cut -f1))"
  curl -sf --http1.1 --retry 5 --retry-all-errors --retry-delay 5 --max-time 900 \
    -X POST \
    -H "Authorization: Bearer $GH_PAT" \
    -H "Content-Type: application/octet-stream" \
    --data-binary "@$asset" \
    "https://uploads.github.com/repos/$REPO/releases/$release_id/assets?name=$asset" >/dev/null
done

# The updater is only as good as this URL, and a silent failure here means
# every installed copy quietly stops updating. Confirm both are actually
# fetchable at the address the AppImage was built to check.
# Size as well as status: a 200 only proves something is served at the
# address, not that it is this build. GitHub accepting a truncated upload
# would pass a status-only check and then fail every client's checksum.
echo "==> Verifying the published URLs"
for asset in "${ASSETS[@]}"; do
  url="https://github.com/$REPO/releases/download/$TAG/$asset"
  local_size="$(stat -c %s "$asset")"

  headers="$(curl -sIL "$url" | tr -d '\r')"
  code="$(printf '%s\n' "$headers" | awk '/^HTTP\//{c=$2} END{print c}')"
  served="$(printf '%s\n' "$headers" | awk 'tolower($1)=="content-length:"{n=$2} END{print n}')"

  [ "$code" = "200" ] || { echo "FAILED: $url returned ${code:-no status}" >&2; exit 1; }
  [ "$served" = "$local_size" ] \
    || { echo "FAILED: $url serves ${served:-unknown} bytes, built $local_size." >&2; exit 1; }
  echo "    $code  $served bytes  $url"
done

echo "OK: $TAG updated, and anchored in Gitea so the mirror preserves it."
