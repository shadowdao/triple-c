#!/usr/bin/env bash
#
# build-ssm-plugin.sh [target-triple ...] - build AWS's session-manager-plugin
# FROM SOURCE and put it where Tauri's `bundle.externalBin` expects it:
#
#     app/src-tauri/binaries/session-manager-plugin-<target-triple>[.exe]
#
# The app runs the plugin to connect to workspaces on AWS; bundling it means
# users do not have to install it. We build it ourselves from AWS's Apache-2.0
# source (github.com/aws/session-manager-plugin) rather than redistributing
# AWS's prebuilt binaries, which AWS ships as "AWS Content" under the AWS
# Customer Agreement.
#
# With no argument the host's triple is used (`rustc -vV`). Every target is a
# pure-Go cross-compile (CGO_ENABLED=0), so any host can build any of them; CI
# builds each platform's own targets in its own job. scripts/build-ssm-plugin.ps1
# is the same build for the Windows runner, which has no bash.
#
# Supported triples:
#   x86_64-unknown-linux-gnu   linux/amd64
#   aarch64-unknown-linux-gnu  linux/arm64
#   x86_64-apple-darwin        darwin/amd64
#   aarch64-apple-darwin       darwin/arm64
#   universal-apple-darwin     both darwin slices plus the two thin files,
#                              joined with lipo (`tauri build --target
#                              universal-apple-darwin` compiles each arch and
#                              tauri-build wants each arch's sidecar; the
#                              bundler then wants the universal one)
#   x86_64-pc-windows-msvc     windows/amd64 (for local cross-builds; CI uses
#                              the .ps1)
#
# Trust and reproducibility - the build fails on any miss:
#   1. The source is AWS's tag 1.2.896.0, pinned by COMMIT, downloaded as
#      GitHub's archive of that commit and checked against a pinned SHA-256
#      BEFORE extraction (the SHA-256 is the check; the archive's top
#      directory, which names the commit, must also be there). Only what the
#      build reads is extracted: the vendored SDK's Java `codegen/` tree and its
#      test `snapshot/` files are skipped, which also keeps every path under
#      Windows' MAX_PATH (the .ps1 extracts the same way, so both build the
#      same tree).
#   2. The Go toolchain is a pinned release from go.dev, checked against the
#      SHA-256 go.dev publishes (https://go.dev/dl/?mode=json), before use.
#   3. The build is AWS's own (its makefile: GOPATH mode over the vendored
#      dependencies, `-ldflags "-s -w"`, package sessionmanagerplugin-main),
#      with VERSION set to the tag so `--version` prints 1.2.896.0 (AWS's
#      release pipeline stamps it; the repo's VERSION file says 1.3.0.0), plus
#      what makes it reproducible: -trimpath, no VCS stamping, an empty build
#      id, CGO off, explicit GOAMD64/GOARM64, no inherited GOFLAGS.
#   4. The output must equal a PINNED SHA-256 per target. Go builds are
#      bit-for-bit reproducible across hosts, so every CI runner must produce
#      exactly the binary recorded here; a mismatch means the source, the
#      toolchain or the build changed.
#
# To bump the plugin or Go: change the pins in this script and in
# build-ssm-plugin.ps1, rebuild every target, record the new output hashes
# (both scripts), and update the version/commit lines in
# app/src-tauri/THIRD_PARTY_NOTICES (checked below) and its dependency licences.
#
# Idempotent: a destination that already holds the pinned binary is left alone.

set -euo pipefail

PLUGIN_VERSION="1.2.896.0"
PLUGIN_COMMIT="6430675a44032a551be1937fb27a2d3fdfeb27f7"
SOURCE_URL="https://codeload.github.com/aws/session-manager-plugin/tar.gz/${PLUGIN_COMMIT}"
SOURCE_SHA256="7c95be411ad5f43d14d01460500bfd5a5aa0e28ba94815f8b049af33a2c381b0"

GO_VERSION="1.26.11"
go_pin() { # <goos-goarch of the build host> -> SHA-256 published by go.dev
  case "$1" in
    linux-amd64)  echo 9849f4e04ee35303714e34fbc3ac8ab44ea3e448a95aa182f91e3b9b79c710ec ;;
    linux-arm64)  echo 6c2f29b458b5add0835f18b776f69c8881a6d88428f1484a6da55755e620757e ;;
    darwin-amd64) echo 394baae971332f2bc783d6e2004ae57101479bd3b06ed8209a3fe3570d4ce498 ;;
    darwin-arm64) echo e3a42e67a7a8acf450ebada2b074c7ca863000ca514907b7185b45e8f5cdff57 ;;
    *) return 1 ;;
  esac
}

# target triple -> GOOS GOARCH  SHA-256 of the binary we build
target_pin() {
  case "$1" in
    x86_64-unknown-linux-gnu)  echo "linux amd64 414dafe49a90428cb4271a28ea964ad9242a26cb08a38d9679d808470c3b6b62" ;;
    aarch64-unknown-linux-gnu) echo "linux arm64 79f00cd41536611e2906a9ea703175ab4f06f9b81107a0357b2304c8bfac61dd" ;;
    x86_64-apple-darwin)       echo "darwin amd64 fc61cd7e30bef3524a3681dc6b1a0f472506d9c98bccf62a67865067c9f2f059" ;;
    aarch64-apple-darwin)      echo "darwin arm64 9f3857f85cbcdc88e3ca686f84ac2a794e19e1778ad47ac3294c39e700de08f8" ;;
    x86_64-pc-windows-msvc)    echo "windows amd64 6d428e5fd77e76d9713f0c42b8ec39f2de1031e5f3c567feb73c7fe11b14b226" ;;
    *) return 1 ;;
  esac
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest_dir="$repo_root/app/src-tauri/binaries"
notices="$repo_root/app/src-tauri/THIRD_PARTY_NOTICES"

die() { echo "build-ssm-plugin: $*" >&2; exit 1; }

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# The notices name the version and commit they describe; a bump updates both.
grep -qF "Version:  ${PLUGIN_VERSION}" "$notices" && grep -qF "Commit:   ${PLUGIN_COMMIT}" "$notices" \
  || die "$notices does not name plugin ${PLUGIN_VERSION} at ${PLUGIN_COMMIT} - update it with the bump"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
prepared=false

download() { # <url> <out>
  curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --retry-delay 5 -o "$2" "$1" \
    || die "download failed: $1"
}

# Toolchain and source, once per run, both verified before they are unpacked.
prepare() {
  [ "$prepared" = true ] && return 0

  local host_os host_arch go_sha go_tgz
  case "$(uname -s)" in Linux) host_os=linux ;; Darwin) host_os=darwin ;; *) die "unsupported build host $(uname -s)" ;; esac
  case "$(uname -m)" in x86_64|amd64) host_arch=amd64 ;; arm64|aarch64) host_arch=arm64 ;; *) die "unsupported build host arch $(uname -m)" ;; esac
  go_sha="$(go_pin "$host_os-$host_arch")" || die "no pinned Go for $host_os-$host_arch"
  go_tgz="$work/go.tar.gz"
  echo "Downloading Go ${GO_VERSION} (${host_os}-${host_arch})"
  download "https://go.dev/dl/go${GO_VERSION}.${host_os}-${host_arch}.tar.gz" "$go_tgz"
  [ "$(sha256 "$go_tgz")" = "$go_sha" ] \
    || die "SHA-256 mismatch for the Go toolchain: got $(sha256 "$go_tgz"), pinned $go_sha"
  mkdir -p "$work/toolchain"
  tar -xzf "$go_tgz" -C "$work/toolchain"
  GO="$work/toolchain/go/bin/go"

  local src_tgz="$work/source.tar.gz"
  echo "Downloading session-manager-plugin ${PLUGIN_VERSION} source (${PLUGIN_COMMIT})"
  download "$SOURCE_URL" "$src_tgz"
  [ "$(sha256 "$src_tgz")" = "$SOURCE_SHA256" ] \
    || die "SHA-256 mismatch for the plugin source: got $(sha256 "$src_tgz"), pinned $SOURCE_SHA256"
  mkdir -p "$work/src"
  tar --exclude='*/codegen/*' --exclude='*/snapshot/*' -xzf "$src_tgz" -C "$work/src"
  GO_SPACE="$work/src/session-manager-plugin-${PLUGIN_COMMIT}"
  [ -d "$GO_SPACE/src/sessionmanagerplugin-main" ] || die "the source archive is not the pinned commit's tree"

  # makefile `pre-build`: stamp the version with AWS's own generator.
  printf '%s' "$PLUGIN_VERSION" >"$GO_SPACE/VERSION"
  ( cd "$GO_SPACE" && go_env "$(go_host_os)" "$(go_host_arch)" "$GO" run ./src/version/versiongenerator/version-gen.go >/dev/null )
  grep -qF "const Version = \"${PLUGIN_VERSION}\"" "$GO_SPACE/src/version/version.go" \
    || die "the version stamp did not take"

  # makefile `copy-src`: the code builds in GOPATH mode, from a copy at its
  # import path, with the vendored dependencies as a second GOPATH entry.
  GOPATH_DIR="$work/gopath"
  mkdir -p "$GOPATH_DIR/src/github.com/aws/session-manager-plugin"
  cp -R "$GO_SPACE/src" "$GOPATH_DIR/src/github.com/aws/session-manager-plugin/"
  prepared=true
}

go_host_os() { "$GO" env GOHOSTOS; }
go_host_arch() { "$GO" env GOHOSTARCH; }

# Run with a clean, fully specified Go environment (nothing inherited).
go_env() { # <goos> <goarch> <cmd...>
  local goos="$1" goarch="$2"; shift 2
  env -u GOFLAGS -u GOROOT -u GOBIN -u GOEXPERIMENT \
    GOENV=off GOTOOLCHAIN=local GO111MODULE=off GOPROXY=off GOWORK=off \
    GOPATH="${GOPATH_DIR:-$work/gopath}:${GO_SPACE}/vendor" \
    GOCACHE="$work/cache" GOMODCACHE="$work/modcache" GOTMPDIR= \
    CGO_ENABLED=0 GOOS="$goos" GOARCH="$goarch" GOAMD64=v1 GOARM64=v8.0 \
    "$@"
}

build_thin() { # <triple> <out>
  local triple="$1" out="$2" pin goos goarch want
  pin="$(target_pin "$triple")" || die "unsupported target $triple"
  read -r goos goarch want <<<"$pin"

  if [ -f "$out" ] && [ "$(sha256 "$out")" = "$want" ]; then
    echo "$(basename "$out"): already the pinned ${PLUGIN_VERSION} build"
    return 0
  fi
  prepare
  echo "Building session-manager-plugin ${PLUGIN_VERSION} for ${goos}/${goarch}"
  ( cd "$GOPATH_DIR/src/github.com/aws/session-manager-plugin" && \
    go_env "$goos" "$goarch" "$GO" build -trimpath -buildvcs=false \
      -ldflags "-s -w -buildid=" -o "$work/out-$triple" \
      github.com/aws/session-manager-plugin/src/sessionmanagerplugin-main )
  local got
  got="$(sha256 "$work/out-$triple")"
  [ "$got" = "$want" ] || die "the ${goos}/${goarch} build is not the pinned binary: got $got, pinned $want (source, toolchain or build flags changed)"
  mkdir -p "$(dirname "$out")"
  chmod 0755 "$work/out-$triple"
  mv -f "$work/out-$triple" "$out"
  echo "$(basename "$out"): session-manager-plugin ${PLUGIN_VERSION}"
}

build_universal() {
  local lipo
  lipo="$(command -v lipo || command -v llvm-lipo || true)"
  [ -n "$lipo" ] || die "universal-apple-darwin needs lipo (Xcode command line tools) or llvm-lipo"
  local x86="$dest_dir/session-manager-plugin-x86_64-apple-darwin"
  local arm="$dest_dir/session-manager-plugin-aarch64-apple-darwin"
  local uni="$dest_dir/session-manager-plugin-universal-apple-darwin"
  build_thin x86_64-apple-darwin "$x86"
  build_thin aarch64-apple-darwin "$arm"
  "$lipo" -create "$x86" "$arm" -output "$work/universal"
  # Each slice must come back out byte-identical to its pinned thin binary.
  local pair arch triple
  for pair in x86_64:x86_64-apple-darwin arm64:aarch64-apple-darwin; do
    arch="${pair%%:*}" triple="${pair#*:}"
    "$lipo" "$work/universal" -thin "$arch" -output "$work/slice-$arch"
    [ "$(sha256 "$work/slice-$arch")" = "$(target_pin "$triple" | cut -d' ' -f3)" ] \
      || die "the $arch slice of the universal binary is not the pinned binary"
  done
  chmod 0755 "$work/universal"
  mv -f "$work/universal" "$uni"
  echo "$(basename "$uni"): session-manager-plugin ${PLUGIN_VERSION} (x86_64 + arm64)"
}

targets=("$@")
if [ ${#targets[@]} -eq 0 ]; then
  host="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')"
  [ -n "$host" ] || die "no target given and rustc is not available to name the host's"
  targets=("$host")
fi

for t in "${targets[@]}"; do
  case "$t" in
    universal-apple-darwin) build_universal ;;
    *-windows-*) build_thin "$t" "$dest_dir/session-manager-plugin-$t.exe" ;;
    *) build_thin "$t" "$dest_dir/session-manager-plugin-$t" ;;
  esac
done
