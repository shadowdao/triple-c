#!/bin/sh
# Messages name paths as the user sees them ("~/.claude/..."), deliberately.
# shellcheck disable=SC2088
# Triple-C marketplace sync: applies the payload the app uploaded.
#
# A constant script, shipped inside the app and uploaded next to the payload on
# every sync. Nothing is ever interpolated into it: its only inputs are the
# files under $MARKETPLACE_INCOMING (written by the host) and $HOME. Item keys
# and slugs are re-validated here although the host validated them, and every
# destination path is derived from them rather than taken from the manifest.
#
# Progress and tool output go to stderr. stdout carries exactly one line: the
# JSON report. Exit status is 0 unless HOME is unset; per-item failures are
# reported, never fatal.
set -u

if [ -z "${HOME:-}" ]; then
  echo "triple-c-marketplace-sync: HOME is not set" >&2
  exit 2
fi
PATH="$HOME/.claude/bin:$HOME/.local/bin:$PATH"
export PATH

CLAUDE_DIR="$HOME/.claude"
BASE="$CLAUDE_DIR/triple-c"
INCOMING="${MARKETPLACE_INCOMING:-$BASE/marketplace/incoming}"
LOCK="${MARKETPLACE_LOCK:-/tmp/.triple-c-claude-update.lock}"
STATE="$BASE/marketplace/state.json"
WORK="$BASE/marketplace/work"
SETTINGS="$CLAUDE_DIR/settings.json"
TAB=$(printf '\t')

if ! command -v jq >/dev/null 2>&1; then
  printf '%s\n' '{"errors":["jq is not installed in this container, so marketplace items were not applied"]}'
  exit 0
fi

R=$(mktemp -d 2>/dev/null) || R=""
if [ -z "$R" ] || [ ! -d "$R" ]; then
  printf '%s\n' '{"errors":["a temporary directory could not be created in the container, so marketplace items were not applied"]}'
  exit 0
fi
trap 'rm -rf "$R"' EXIT
for f in installed updated removed skipped errors newstate new_slugs final_slugs \
  hook_pending hook_removals plugin_items; do
  : >"$R/$f"
done

report() { printf '%s\n' "$2" >>"$R/$1"; }
skip() { printf '%s\t%s\n' "$1" "$2" >>"$R/skipped"; }
fail() { printf '%s\n' "$1" >>"$R/errors"; }
record() { printf '%s\t%s\n' "$1" "$2" >>"$R/newstate"; }

emit_report() {
  jq -cn \
    --rawfile i "$R/installed" --rawfile u "$R/updated" --rawfile d "$R/removed" \
    --rawfile s "$R/skipped" --rawfile e "$R/errors" '
    def lines: split("\n") | map(select(length > 0));
    { installed: ($i | lines), updated: ($u | lines), removed: ($d | lines),
      skipped: ($s | lines | map(split("\t") | { item: .[0], reason: (.[1:] | join("\t")) })),
      errors: ($e | lines) }'
}

valid_key() {
  case "$1" in
    '' | [!A-Za-z0-9]* | *[!A-Za-z0-9._-]*) return 1 ;;
  esac
  [ "${#1}" -le 64 ]
}

valid_slug() {
  case "$1" in
    '' | -* | *[!a-z0-9-]*) return 1 ;;
  esac
  [ "${#1}" -le 64 ]
}

valid_commit() {
  case "$1" in
    '' | *[!0-9a-f]*) return 1 ;;
  esac
  [ "${#1}" -eq 40 ]
}

# Run `claude` serialised with the entrypoint's and every session's
# `claude update`, which rewrite ~/.claude/bin under the same lock.
claude_cmd() {
  if command -v flock >/dev/null 2>&1; then
    flock -w 120 "$LOCK" claude "$@" </dev/null >&2
  else
    claude "$@" </dev/null >&2
  fi
}

# State ids are "<kind>:<key>", except plugins: "plugin:<slug>/<key>", since
# two marketplaces may ship a plugin of the same name. Reports keep
# "<kind>:<key>" for every kind. $OLD is the state as read at the start
# (legacy "plugin:<key>" records migrated); $STATE is written once, at the end.
OLD="$R/state.json"
owned() { jq -e --arg id "$1" '.items | has($id)' "$OLD" >/dev/null 2>&1; }
prev_commit() { jq -r --arg id "$1" '.items[$id].commit // ""' "$OLD"; }
# Every state id the manifest names with string fields, well-formed or
# not: a selected item that failed this run must not be removed.
in_manifest() { grep -qxF "$1" "$R/manifest_ids"; }
carry_forward() { record "$1" "$(jq -c --arg id "$1" '.items[$id]' "$OLD")"; }
# $1 = installed|updated|none for this id at this commit.
outcome_of() {
  p=$(prev_commit "$1")
  if [ -z "$p" ]; then
    echo installed
  elif [ "$p" != "$2" ]; then
    echo updated
  else
    echo none
  fi
}
outcome() {
  o=$(outcome_of "$1" "$2")
  [ "$o" = none ] || report "$o" "$1"
}
# Something is in the way at a user-owned location (dangling links included).
occupied() { [ -e "$1" ] || [ -L "$1" ]; }

# The one place a destination is derived; removal never trusts a stored path.
item_path() {
  case "$1" in
    agent | command) printf '%s\n' "$CLAUDE_DIR/${1}s/$2.md" ;;
    skill) printf '%s\n' "$CLAUDE_DIR/skills/$2" ;;
    hook) printf '%s\n' "$BASE/hooks/$2" ;;
    *) return 1 ;;
  esac
}

malformed() {
  rm -rf "$WORK"
  fail "$1"
  emit_report
  exit 0
}

# ── Unpack ───────────────────────────────────────────────────────────────────
if [ ! -f "$INCOMING/payload.tar" ]; then
  fail "no payload was uploaded"
  emit_report
  exit 0
fi
mkdir -p "$BASE/marketplace" "$BASE/hooks" "$BASE/plugins"
rm -rf "$WORK"
mkdir -p "$WORK"
if ! tar -xf "$INCOMING/payload.tar" -C "$WORK" >&2; then
  rm -f "$INCOMING/payload.tar"
  fail "the payload could not be unpacked"
  emit_report
  exit 0
fi
rm -f "$INCOMING/payload.tar"
# The host never packs links (they make an item invalid); refuse any that
# arrive rather than copy through them.
if [ -n "$(find "$WORK" -type l -print | head -n 1)" ]; then
  rm -rf "$WORK"
  fail "the payload contains a symbolic link, so it was not applied"
  emit_report
  exit 0
fi
MANIFEST="$WORK/manifest.json"
if ! jq -e '.version == 1' "$MANIFEST" >/dev/null 2>&1; then
  malformed "the payload manifest is missing or has an unsupported version"
fi
# Nothing is changed (and, above all, nothing removed) unless the manifest is
# structurally sound and every extraction below succeeds.
# `held` (optional): state ids of installs the host could not build this time;
# they are kept exactly like a selected item that failed here.
if ! jq -e '(.items | type) == "array" and (.plugin_marketplaces | type) == "array"
    and ((.held // []) | type == "array" and all(.[]; type == "string"))' \
  "$MANIFEST" >/dev/null 2>&1; then
  malformed "the payload manifest is malformed, so nothing was changed"
fi
# One line per item. Fields carry a "_" prefix so an empty one cannot make
# `read` shift the rest (tab is IFS whitespace); @tsv escapes tabs/newlines.
# A malformed item becomes a "bad" line instead of aborting the extraction.
if ! {
  jq -r '
    .items[]
    | if type == "object" and (.kind | type) == "string" and (.key | type) == "string"
       and (.commit | type) == "string"
    then ["ok", .kind, .key, .commit, (if (.slug | type) == "string" then .slug else "" end)]
    else ["bad",
          (if type == "object" then .kind | tostring else "?" end),
          (if type == "object" then .key | tostring else "?" end), "", ""]
    end
  | map("_" + .) | @tsv' "$MANIFEST" >"$R/items.tsv" &&
  jq -r '.items[] | objects | select((.kind | type) == "string" and (.key | type) == "string")
    | [if .kind == "plugin" and (.slug | type) == "string"
       then "plugin:" + .slug + "/" + .key else .kind + ":" + .key end] | @tsv' \
    "$MANIFEST" >"$R/manifest_ids" &&
  jq -r '(.held // [])[] | [.] | @tsv' "$MANIFEST" >>"$R/manifest_ids" &&
  jq -r '.plugin_marketplaces[]
    | if type == "object" and (.slug | type) == "string" then .slug else "" end
    | [.] | @tsv' "$MANIFEST" >"$R/new_slugs"
}; then
  malformed "the payload manifest could not be read, so nothing was changed"
fi
if ! jq -e '(.items | type) == "object"' "$STATE" >/dev/null 2>&1; then
  printf '%s\n' '{"version":1,"items":{},"plugin_marketplaces":[]}' >"$STATE"
fi
# Records from before plugins were tracked per marketplace ("plugin:<key>")
# carry their slug: rename them so they are neither reinstalled nor orphaned.
# One without a string slug keeps its id and is dropped as unrecognised below.
if ! jq '.items |= with_entries(
    if (.key | startswith("plugin:")) and (.key | contains("/") | not)
       and (.value | type) == "object" and (.value.slug | type) == "string"
    then .key = "plugin:" + .value.slug + "/" + (.key | ltrimstr("plugin:"))
    else . end)' "$STATE" >"$OLD" 2>/dev/null; then
  malformed "the marketplace state could not be read, so nothing was changed"
fi

# ── Agents, skills, commands, hooks ──────────────────────────────────────────
while IFS="$TAB" read -r status kind key commit slug; do
  status=${status#_} kind=${kind#_} key=${key#_} commit=${commit#_} slug=${slug#_}
  id="$kind:$key"
  if [ "$status" != ok ]; then skip "$id" "malformed manifest entry"; continue; fi
  if ! valid_key "$key"; then skip "$id" "invalid item name"; continue; fi
  if ! valid_commit "$commit"; then skip "$id" "invalid commit"; continue; fi
  case "$kind" in
    plugin)
      # Applied per plugin marketplace below.
      printf '%s\t%s\t%s\n' "_$key" "_$commit" "_$slug" >>"$R/plugin_items"
      ;;
    agent | command)
      dir="$CLAUDE_DIR/${kind}s"
      src="$WORK/${kind}s/$key.md"
      dest=$(item_path "$kind" "$key")
      if [ ! -f "$src" ]; then fail "$id: missing from the payload"; continue; fi
      if occupied "$dest" && ! owned "$id"; then
        skip "$id" "~/.claude/${kind}s/$key.md already exists and was not installed by Triple-C"
        continue
      fi
      if ! { mkdir -p "$dir" && cp "$src" "$dest.tmp.$$" && mv -f "$dest.tmp.$$" "$dest"; }; then
        rm -f "$dest.tmp.$$"
        fail "$id: could not write $dest"
        continue
      fi
      outcome "$id" "$commit"
      record "$id" "$(jq -cn --arg c "$commit" --arg p "$dest" '{commit: $c, path: $p}')"
      ;;
    skill)
      dir="$CLAUDE_DIR/skills"
      src="$WORK/skills/$key"
      dest=$(item_path skill "$key")
      if [ ! -d "$src" ]; then fail "$id: missing from the payload"; continue; fi
      if occupied "$dest" && ! owned "$id"; then
        skip "$id" "~/.claude/skills/$key already exists and was not installed by Triple-C"
        continue
      fi
      if ! { mkdir -p "$dir" && rm -rf "$dest" && cp -R "$src" "$dest"; }; then
        fail "$id: could not write $dest"
        continue
      fi
      outcome "$id" "$commit"
      record "$id" "$(jq -cn --arg c "$commit" --arg p "$dest" '{commit: $c, path: $p}')"
      ;;
    hook)
      src="$WORK/hooks/$key"
      dest=$(item_path hook "$key")
      entries=$(jq -c --arg k "$key" \
        'first(.items[] | objects | select(.kind == "hook" and .key == $k) | .settings) // {}' "$MANIFEST")
      if ! printf '%s' "$entries" | jq -e 'type == "object" and all(.[]; type == "array")' >/dev/null 2>&1; then
        skip "$id" "its hook settings are not an object of arrays"
        continue
      fi
      if [ ! -d "$src" ]; then fail "$id: missing from the payload"; continue; fi
      if ! { rm -rf "$dest" && cp -R "$src" "$dest"; }; then
        fail "$id: could not write $dest"
        continue
      fi
      # Reported only once its entries are in settings.json (see below).
      printf '%s\t%s\n' "$(outcome_of "$id" "$commit")" "$id" >>"$R/hook_pending"
      record "$id" "$(jq -cn --arg c "$commit" --arg p "$dest" --argjson e "$entries" \
        '{commit: $c, path: $p, entries: $e}')"
      ;;
    *)
      skip "$id" "unknown item kind"
      ;;
  esac
done <"$R/items.tsv"

# ── Removals (non-plugin) ────────────────────────────────────────────────────
cut -f1 "$R/newstate" >"$R/new_ids"
jq -r '.items | keys[]' "$OLD" >"$R/old_ids"
while read -r id; do
  case "$id" in plugin:*) continue ;; esac
  if grep -qxF "$id" "$R/new_ids"; then continue; fi
  # Still selected but failed this run: keep the old files and record.
  if in_manifest "$id"; then carry_forward "$id"; continue; fi
  # Only an exact "<kind>:<key>" with a known kind names a path; anything
  # else in state is dropped without deleting anything.
  case "$id" in
    agent:* | skill:* | command:* | hook:*)
      kind=${id%%:*}
      key=${id#*:}
      ;;
    *) kind="" key="" ;;
  esac
  if [ -z "$kind" ] || ! valid_key "$key" || ! path=$(item_path "$kind" "$key"); then
    fail "$id: dropped an unrecognised record from the marketplace state"
    continue
  fi
  if [ "$kind" = hook ]; then
    # Removed once its entries are out of settings.json (see below).
    printf '%s\n' "$id" >>"$R/hook_removals"
    continue
  fi
  if rm -rf "$path"; then report removed "$id"; else fail "$id: could not remove $path"; fi
done <"$R/old_ids"

# ── Hook entries in settings.json ────────────────────────────────────────────
# shellcheck disable=SC2016 # jq program, not shell
MERGE_ENTRIES='[.[] | .entries? // empty]
  | reduce .[] as $e ({}; reduce ($e | to_entries[]) as $x (.; .[$x.key] += $x.value))'
OLD_HOOKS=$(jq -c "[.items[]] | $MERGE_ENTRIES" "$OLD")
NEW_HOOKS=$(cut -f2- "$R/newstate" | jq -cs "$MERGE_ENTRIES")
HOOKS_FAILED=0
if [ "$OLD_HOOKS" != "{}" ] || [ "$NEW_HOOKS" != "{}" ]; then
  # A dotfiles symlink stays a symlink: write through to its target.
  target="$SETTINGS"
  if [ -L "$SETTINGS" ]; then
    target=$(readlink -f "$SETTINGS" 2>/dev/null) || target=""
  fi
  tmp="$target.tmp.$$"
  # settings.json may hold secrets and the entrypoint keeps it 0600: create
  # the replacement private and keep it that way (pre-flight N11).
  saved_umask=$(umask)
  umask 077
  if [ -z "$target" ] || { [ -e "$target" ] && [ ! -f "$target" ]; }; then
    HOOKS_FAILED=1
    fail "~/.claude/settings.json is not a regular file, so hook changes were not applied"
  elif [ -f "$target" ] && ! jq -s '
      if length == 0 then {}
      elif length == 1 and (.[0] | type) == "object" then .[0]
      else error("not a JSON object") end' "$target" >"$R/current.json" 2>/dev/null; then
    HOOKS_FAILED=1
    fail "~/.claude/settings.json is not a JSON object, so hook changes were not applied"
  else
    # Missing, empty and whitespace-only files all read as {}.
    [ -f "$target" ] || printf '{}\n' >"$R/current.json"
    if jq --argjson old "$OLD_HOOKS" --argjson new "$NEW_HOOKS" '
        def remove_first($x):
          (to_entries | map(select(.value == $x)) | first(.[].key) // null) as $i
          | if $i == null then . else del(.[$i]) end;
        reduce ($old | to_entries[]) as $ev (.;
          if (.hooks[$ev.key] | type) == "array"
          then reduce $ev.value[] as $g (.; .hooks[$ev.key] |= remove_first($g))
          else . end)
        | reduce ($new | to_entries[]) as $ev (.;
            .hooks[$ev.key] = ((.hooks[$ev.key] // []) + $ev.value))
        | if (.hooks | type) == "object" then .hooks |= with_entries(select(.value != [])) else . end
        | if .hooks == {} then del(.hooks) else . end
      ' "$R/current.json" >"$tmp" 2>/dev/null &&
      jq -e 'type == "object"' "$tmp" >/dev/null 2>&1 &&
      mv -f "$tmp" "$target"; then
      chmod 600 "$target" ||
        fail "~/.claude/settings.json was updated but could not be made private (chmod 600)"
    else
      rm -f "$tmp"
      HOOKS_FAILED=1
      fail "~/.claude/settings.json could not be updated, so hook changes were not applied"
    fi
  fi
  umask "$saved_umask"
fi
if [ "$HOOKS_FAILED" = 0 ]; then
  while IFS="$TAB" read -r o id; do
    [ "$o" = none ] || report "$o" "$id"
  done <"$R/hook_pending"
  while read -r id; do
    key=${id#hook:}
    if rm -rf "$(item_path hook "$key")"; then report removed "$id"; else fail "$id: could not remove its files"; fi
  done <"$R/hook_removals"
fi

# ── Plugins ──────────────────────────────────────────────────────────────────
jq -r '.plugin_marketplaces[]?' "$OLD" >"$R/old_slugs"
while read -r slug; do
  if ! valid_slug "$slug"; then fail "invalid plugin marketplace name"; continue; fi
  mname="triple-c-$slug"
  dest="$BASE/plugins/$slug"
  if ! { rm -rf "$dest" && cp -R "$WORK/plugins/$slug" "$dest"; }; then
    fail "$mname: could not write $dest"
    continue
  fi
  if grep -qxF "$slug" "$R/old_slugs"; then
    claude_cmd plugin marketplace update "$mname" || fail "$mname: marketplace update failed"
  elif ! claude_cmd plugin marketplace add "$dest"; then
    claude_cmd plugin marketplace update "$mname" || { fail "$mname: could not be registered"; continue; }
  fi
  printf '%s\n' "$slug" >>"$R/final_slugs"
  while IFS="$TAB" read -r key commit pslug; do
    key=${key#_} commit=${commit#_} pslug=${pslug#_}
    [ "$pslug" = "$slug" ] || continue
    id="plugin:$key"
    sid="plugin:$slug/$key"
    p=$(prev_commit "$sid")
    if [ -z "$p" ]; then
      claude_cmd plugin install "$key@$mname" || { fail "$id ($mname): install failed"; continue; }
      report installed "$id"
    elif [ "$p" != "$commit" ]; then
      claude_cmd plugin uninstall "$key@$mname"
      claude_cmd plugin install "$key@$mname" || { fail "$id ($mname): reinstall failed"; continue; }
      report updated "$id"
    fi
    record "$sid" "$(jq -cn --arg c "$commit" --arg s "$slug" '{commit: $c, slug: $s}')"
  done <"$R/plugin_items"
done <"$R/new_slugs"

# Plugins no longer selected.
while read -r id; do
  case "$id" in plugin:*) ;; *) continue ;; esac
  if grep -qxF "$id" "$R/new_ids" || cut -f1 "$R/newstate" | grep -qxF "$id"; then continue; fi
  if in_manifest "$id"; then carry_forward "$id"; continue; fi
  # Name and marketplace come from the id alone ("plugin:<slug>/<key>").
  rest=${id#plugin:}
  case "$rest" in
    */*) slug=${rest%%/*} key=${rest#*/} ;;
    *) slug="" key="" ;;
  esac
  if ! valid_key "$key" || ! valid_slug "$slug"; then
    fail "$id: dropped an unrecognised record from the marketplace state"
    continue
  fi
  if claude_cmd plugin uninstall "$key@triple-c-$slug"; then
    report removed "plugin:$key"
  else
    fail "plugin:$key (triple-c-$slug): uninstall failed"
    carry_forward "$id"
  fi
done <"$R/old_ids"

# Plugin marketplaces with nothing left in them.
cut -f2- "$R/newstate" | jq -r 'select(has("slug")) | .slug' >>"$R/final_slugs"
while read -r slug; do
  if grep -qxF "$slug" "$R/final_slugs"; then continue; fi
  valid_slug "$slug" || continue
  claude_cmd plugin marketplace remove "triple-c-$slug" || fail "triple-c-$slug: could not be removed"
  rm -rf "$BASE/plugins/$slug"
done <"$R/old_slugs"

# ── State ────────────────────────────────────────────────────────────────────
jq -Rn '[inputs | split("\t") | { key: .[0], value: (.[1:] | join("\t") | fromjson) }] | from_entries' \
  <"$R/newstate" >"$R/items.json"
if [ "$HOOKS_FAILED" = 1 ]; then
  # settings.json still holds the old entries, so the old records stay true.
  jq -s '.[0] as $new | .[1].items as $old
    | ($new | with_entries(select(.key | startswith("hook:") | not)))
      + ($old | with_entries(select(.key | startswith("hook:"))))' \
    "$R/items.json" "$OLD" >"$R/items2.json" && mv -f "$R/items2.json" "$R/items.json"
fi
if jq -n --slurpfile it "$R/items.json" --rawfile sl "$R/final_slugs" \
  '{ version: 1, items: $it[0], plugin_marketplaces: ($sl | split("\n") | map(select(length > 0)) | unique) }' \
  >"$STATE.tmp.$$"; then
  mv -f "$STATE.tmp.$$" "$STATE"
else
  rm -f "$STATE.tmp.$$"
  fail "the marketplace state could not be saved"
fi

rm -rf "$WORK"
emit_report
