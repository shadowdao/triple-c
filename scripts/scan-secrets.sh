#!/bin/sh
# Refuse to let a live credential into the repository.
#
# Written after one got in: `the_custom_env_fingerprint_never_carries_the_value`
# used the maintainer's real Gitea site-admin token as its fixture. It survived
# 92 commits and fourteen days in a public mirror, past five audit rounds and two
# independent reviews — because every one of those looked at the code under
# change, and this sat in a test nobody had reason to open. A grep would have
# caught it on the first day. This is that grep.
#
# Usage:
#   scan-secrets.sh --staged        what `git commit` is about to record (the hook)
#   scan-secrets.sh --range A..B    every line added between two commits (CI)
#   scan-secrets.sh --tracked       every tracked file, as it stands now
#
# Exit 0 clean, 1 on a finding, 2 on misuse.
#
# ## Why it scans *added lines* and not the whole file
#
# The repository already contains long opaque strings — 317 literals of 32+
# characters, almost all of them legitimate. A scanner that failed on those would
# be turned off within a day, which is the normal way this kind of check dies.
# Judging only what a commit *adds* keeps the signal where a person can act on it.
#
# ## Escape hatch
#
# A line carrying `pragma: allowlist secret` is skipped. Deliberately wordy: it
# should be uncomfortable enough to type that it is read as a claim, and it
# leaves something greppable behind.

set -eu

MODE="${1:---staged}"
RANGE="${2:-}"

case "$MODE" in
  --staged)  ADDED=$(git diff --cached --unified=0 --no-color -- . 2>/dev/null || true) ;;
  --range)   [ -n "$RANGE" ] || { echo "scan-secrets: --range needs A..B" >&2; exit 2; }
             ADDED=$(git diff --unified=0 --no-color "$RANGE" -- . 2>/dev/null || true) ;;
  --tracked)
             # `grep -Iq .` first: without it a binary blob's bytes reach the
             # rules below, and GNU grep answers "binary file matches" instead
             # of the line — so a real finding inside one would be reported as
             # a sentence nobody can act on, and a stray NUL can end the scan
             # early. Text files only; binaries are not where source secrets
             # live, and `--staged` never sees them either (git emits
             # "Binary files differ", not content).
             ADDED=$(git ls-files -z \
               | xargs -0 -I{} sh -c 'grep -Iq . "{}" 2>/dev/null && sed "s/^/+/" "{}" 2>/dev/null' \
               || true) ;;
  *)         echo "scan-secrets: unknown mode $MODE" >&2; exit 2 ;;
esac

# Only added lines; drop diff headers (+++ b/path) so a filename never matches.
CANDIDATES=$(printf '%s\n' "$ADDED" \
  | grep '^+' \
  | grep -v '^+++' \
  | grep -v 'pragma: allowlist secret' \
  || true)

[ -n "$CANDIDATES" ] || exit 0

FOUND=0
report() {
  FOUND=1
  printf '\n  %s\n' "$1"
  printf '%s\n' "$2" | sed 's/^/    /' | cut -c1-160
}

# --- Rule 1: vendor-issued credentials. Shape alone identifies these, so there
# --- is no false-positive story to tell and no identifier context needed.
VENDOR=$(printf '%s\n' "$CANDIDATES" | grep -nE \
  'gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|glpat-[A-Za-z0-9_-]{20,}|xox[baprs]-[A-Za-z0-9-]{10,}|sk-[A-Za-z0-9]{32,}|AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|ya29\.[A-Za-z0-9_-]{20,}|AIza[0-9A-Za-z_-]{35}|npm_[A-Za-z0-9]{36}|dckr_pat_[A-Za-z0-9_-]{20,}' \
  || true)
[ -n "$VENDOR" ] && report "A vendor-issued credential (its prefix identifies the provider):" "$VENDOR"

# --- Rule 2: private key material.
KEYS=$(printf '%s\n' "$CANDIDATES" | grep -nE -- '-----BEGIN [A-Z ]*PRIVATE KEY-----' || true)
[ -n "$KEYS" ] && report "Private key material:" "$KEYS"

# --- Rule 3: an opaque literal assigned to a secret-shaped name.
#
# This is the rule that would have caught the Gitea token — `let secret =
# "<40 hex>"`. Both halves are required, and that is what keeps it usable:
# the *name* must read as a credential, and the *whole literal* must be hex or
# base64 with no word structure. `"aws-secret-access-key"` is a keychain key
# name sitting right next to the word `secret`, and its hyphens are what keep it
# out; measured against the tree, name-proximity alone flagged four lines of
# which three were that shape.
OPAQUE=$(printf '%s\n' "$CANDIDATES" | grep -niE \
  '(secret|token|api[_-]?key|apikey|passwo?rd|passwd|credential|auth[_-]?(key|token))[^A-Za-z0-9]{0,12}[:=][^"'"'"']{0,12}["'"'"']([0-9a-fA-F]{32,}|[A-Za-z0-9+/]{40,}={0,2})["'"'"']' \
  || true)
[ -n "$OPAQUE" ] && report "An opaque literal assigned to a secret-shaped name:" "$OPAQUE"

if [ "$FOUND" -eq 1 ]; then
  cat >&2 <<'MSG'

  ────────────────────────────────────────────────────────────────────────
  Refusing the commit: it adds something shaped like a live credential.

  If it IS live: do not amend and move on. Rotate it first — anything that
  reaches a branch is on the mirror, and the mirror is public.

  If it is genuinely not a secret — a fixture, a public key id, test data —
  make the literal obviously fake ("not-a-real-token-0000…"), or append
  `pragma: allowlist secret` to the line to say so on the record.
  ────────────────────────────────────────────────────────────────────────
MSG
  exit 1
fi
exit 0
