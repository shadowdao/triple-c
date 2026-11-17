#!/usr/bin/env bash
#
# fetch-ssm-plugin.sh [target-triple ...] - put AWS's session-manager-plugin
# where Tauri's `bundle.externalBin` expects it:
#
#     app/src-tauri/binaries/session-manager-plugin-<target-triple>
#
# The app runs the plugin to connect to workspaces on AWS, and bundling it means
# users do not have to install it. With no argument the host's triple is used
# (`rustc -vV`). Windows is scripts/fetch-ssm-plugin.ps1.
#
# Supported triples:
#   x86_64-unknown-linux-gnu   ubuntu_64bit/session-manager-plugin.deb
#   aarch64-unknown-linux-gnu  ubuntu_arm64/session-manager-plugin.deb
#   x86_64-apple-darwin        mac/sessionmanager-bundle.zip
#   aarch64-apple-darwin       mac_arm64/sessionmanager-bundle.zip
#   universal-apple-darwin     both macOS slices, plus the two thin files, joined
#                              with lipo (`tauri build --target
#                              universal-apple-darwin` compiles each arch, and
#                              tauri-build wants each arch's sidecar, then the
#                              bundler wants the universal one)
#
# Trust, in order, and the build fails on any miss:
#   1. A PINNED version, downloaded only from AWS's own S3 path.
#   2. A PINNED SHA-256 of every downloaded artifact, checked BEFORE anything is
#      extracted from it.
#   3. For the Linux .deb, AWS's detached GPG signature (.sig), checked with
#      gpgv against the key committed as scripts/aws-ssm-plugin-signer.asc, whose
#      fingerprint is pinned here too. AWS publishes no .sig for the macOS zips
#      (403); those binaries carry AWS's Developer ID signature instead.
#   4. A PINNED SHA-256 of the extracted binary itself.
#
# To bump the version: download every artifact of the new version, check the
# .deb signatures with the committed key, and only then record the new hashes
# (and the version in app/src-tauri/THIRD_PARTY_NOTICES, which this script
# checks). Do the same in fetch-ssm-plugin.ps1.
#
# Idempotent: a destination that already holds the pinned binary is left alone,
# so running it before every build costs nothing.

set -euo pipefail

VERSION="1.2.896.0"
BASE_URL="https://s3.amazonaws.com/session-manager-downloads/plugin/${VERSION}"
# AWS SSM Session Manager <session-manager-plugin-signer@amazon.com>, as
# published at https://docs.aws.amazon.com/systems-manager/latest/userguide/install-plugin-linux-verify-signature.html
SIGNER_FPR="7959637124CE093AD501D47A2C4D4AFF6F6757EE"

# artifact path (under BASE_URL) | SHA-256 of the artifact | SHA-256 of the binary in it
pin_for() {
  case "$1" in
    x86_64-unknown-linux-gnu)
      echo "ubuntu_64bit/session-manager-plugin.deb|53f51fcc67314a4bf85888a38a3967cd41f8d0f28a60a9469a63acf935ba713e|6f6971b0ea322215cb8b316cd290c09a13c916114149a6b4c65358c232be6130" ;;
    aarch64-unknown-linux-gnu)
      echo "ubuntu_arm64/session-manager-plugin.deb|914eeca87b18b55356c58be197080f73742b3ca60d3823f0a45670dc9056aa9c|3f0efc482ce165fcf60a1ec44b40a700d48e7ec4b88cf0c339dd40d1620e98bf" ;;
    x86_64-apple-darwin)
      echo "mac/sessionmanager-bundle.zip|66e826fd48a6a6d06ad06b0892805986f7c455295b61974f99cb2a43d29624d7|e6e94c116003889244c5455a6dcd96ea584d2c6b6d6d719c3956d305429feb8b" ;;
    aarch64-apple-darwin)
      echo "mac_arm64/sessionmanager-bundle.zip|05c5ef53fb262de2fa1b46cf457890ee4152ec1dd8b2fdd94d028fa9b885ff36|71258c3f7d5c15603a3b75bc8dfd62f595e8ec6ef3b8132ab59a005eebb7fa35" ;;
    *) return 1 ;;
  esac
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest_dir="$repo_root/app/src-tauri/binaries"
signer_key="$repo_root/scripts/aws-ssm-plugin-signer.asc"
notices="$repo_root/app/src-tauri/THIRD_PARTY_NOTICES"

die() { echo "fetch-ssm-plugin: $*" >&2; exit 1; }

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# The notices name the version they reproduce; a bump must update both.
grep -qF "Version:  ${VERSION}" "$notices" \
  || die "$notices does not name plugin ${VERSION} - update it with the version bump"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

download() { # <path under BASE_URL> <output file>
  curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 5 \
    -o "$2" "${BASE_URL}/$1" || die "download failed: ${BASE_URL}/$1"
}

verify_deb_signature() { # <deb> <sig>
  command -v gpg >/dev/null 2>&1 || die "gpg is required to check AWS's signature on the .deb"
  command -v gpgv >/dev/null 2>&1 || die "gpgv is required to check AWS's signature on the .deb"
  local keyring="$work/aws-signer.gpg" status="$work/gpgv.status"
  [ -f "$keyring" ] || gpg --batch --quiet --dearmor <"$signer_key" >"$keyring"
  # VALIDSIG carries the primary key's fingerprint as its last field; a good
  # signature from any other key in the keyring (there is none) would not match.
  gpgv --status-fd 3 --keyring "$keyring" "$2" "$1" 3>"$status" >/dev/null 2>&1 \
    || die "AWS's GPG signature does not verify for $(basename "$1")"
  grep -q "^\[GNUPG:\] VALIDSIG .* ${SIGNER_FPR}\$" "$status" \
    || die "the .deb is not signed by the pinned AWS key ${SIGNER_FPR}"
}

# Extract the binary of one thin triple into <out>, every check included.
fetch_thin() { # <triple> <out>
  local triple="$1" out="$2" pin artifact artifact_sha bin_sha file
  pin="$(pin_for "$triple")" || die "unsupported target $triple"
  IFS='|' read -r artifact artifact_sha bin_sha <<<"$pin"

  if [ -f "$out" ] && [ "$(sha256 "$out")" = "$bin_sha" ]; then
    echo "$(basename "$out"): already the pinned ${VERSION} binary"
    return 0
  fi

  file="$work/${triple}-$(basename "$artifact")"
  echo "Downloading ${BASE_URL}/${artifact}"
  download "$artifact" "$file"
  [ "$(sha256 "$file")" = "$artifact_sha" ] \
    || die "SHA-256 mismatch for ${artifact}: got $(sha256 "$file"), pinned ${artifact_sha}"

  local x="$work/${triple}-x"
  mkdir -p "$x"
  case "$artifact" in
    *.deb)
      download "${artifact}.sig" "$file.sig"
      verify_deb_signature "$file" "$file.sig"
      # A .deb is an ar archive; take only data.tar.* and only the one binary.
      ( cd "$x" && ar x "$file" )
      local data
      data="$(cd "$x" && ls data.tar.* 2>/dev/null | head -n1)"
      [ -n "$data" ] || die "no data.tar.* in ${artifact}"
      tar -xf "$x/$data" -C "$x" usr/local/sessionmanagerplugin/bin/session-manager-plugin \
        || die "the binary is not where AWS's .deb used to put it"
      mv "$x/usr/local/sessionmanagerplugin/bin/session-manager-plugin" "$x/plugin"
      ;;
    *.zip)
      unzip -p "$file" sessionmanager-bundle/bin/session-manager-plugin >"$x/plugin" \
        || die "the binary is not where AWS's bundle zip used to put it"
      ;;
  esac
  [ "$(sha256 "$x/plugin")" = "$bin_sha" ] \
    || die "SHA-256 mismatch for the binary in ${artifact}: got $(sha256 "$x/plugin"), pinned ${bin_sha}"

  mkdir -p "$(dirname "$out")"
  chmod 0755 "$x/plugin"
  mv -f "$x/plugin" "$out"
  echo "$(basename "$out"): session-manager-plugin ${VERSION}"
}

fetch_universal() {
  local lipo
  lipo="$(command -v lipo || command -v llvm-lipo || true)"
  [ -n "$lipo" ] || die "universal-apple-darwin needs lipo (Xcode command line tools) or llvm-lipo"
  local x86="$dest_dir/session-manager-plugin-x86_64-apple-darwin"
  local arm="$dest_dir/session-manager-plugin-aarch64-apple-darwin"
  local uni="$dest_dir/session-manager-plugin-universal-apple-darwin"
  fetch_thin x86_64-apple-darwin "$x86"
  fetch_thin aarch64-apple-darwin "$arm"
  "$lipo" -create "$x86" "$arm" -output "$work/universal"
  # Each slice must come back out byte-identical to its pinned thin binary, so
  # the universal file holds exactly AWS's two binaries (and their signatures).
  local arch thin
  for arch in x86_64 arm64; do
    "$lipo" "$work/universal" -thin "$arch" -output "$work/slice-$arch"
  done
  for arch in x86_64:x86_64-apple-darwin arm64:aarch64-apple-darwin; do
    thin="$(pin_for "${arch#*:}" | cut -d'|' -f3)"
    [ "$(sha256 "$work/slice-${arch%%:*}")" = "$thin" ] \
      || die "the ${arch%%:*} slice of the universal binary is not the pinned binary"
  done
  chmod 0755 "$work/universal"
  mv -f "$work/universal" "$uni"
  echo "$(basename "$uni"): session-manager-plugin ${VERSION} (x86_64 + arm64)"
}

targets=("$@")
if [ ${#targets[@]} -eq 0 ]; then
  host="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')"
  [ -n "$host" ] || die "no target given and rustc is not available to name the host's"
  targets=("$host")
fi

for t in "${targets[@]}"; do
  case "$t" in
    universal-apple-darwin) fetch_universal ;;
    *-windows-*) die "$t: use scripts/fetch-ssm-plugin.ps1 on Windows" ;;
    *) fetch_thin "$t" "$dest_dir/session-manager-plugin-$t" ;;
  esac
done
