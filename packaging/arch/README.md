# Arch / CachyOS package

`PKGBUILD` here is the AUR `triple-c-bin` package's template — see triple-c#34
(the "I would like to also have an Arch/CachyOS native version" part of it).

## Why "-bin"

It repackages the same `.deb` `build-app.yml` already produces, rather than
building from source. That means `makepkg` never needs a Rust toolchain,
Node, or the dozen `-dev` packages CLAUDE.md lists for building Triple-C
itself — and a user gets exactly the binary the project ships and tests,
built on Ubuntu 24.04 in CI. Verified end to end against a real release
(v0.4.14): downloaded the actual `.deb`, confirmed every `depends` entry
against a real `ldd` of the actual binary (two packages that looked right
from Tauri's own docs — `pango`, `libayatana-appindicator` — turned out not
to be real dependencies of *this* binary and were dropped), and ran a real
`makepkg`/`namcap`/`pacman -U` cycle rather than guessing at the shape.

## Publishing

`.gitea/workflows/publish-aur-package.yml` does the actual work: given a
version (or "latest" if none is given), it finds that release's real Linux
asset on GitHub, downloads it, computes real checksums, renders this
template into a version-specific PKGBUILD, validates it with `makepkg` and
`namcap` inside a real Arch container, and pushes the result to AUR.

It is `workflow_dispatch`-only, deliberately — see the workflow file's own
header comment for why an automatic trigger isn't safe here (the same reason
`sync-release.yml` didn't work and was removed in triple-c#32).

**Before it can push anything**, an AUR account has to exist and the
`triple-c-bin` package has to have been created (or you added as a
co-maintainer) under it — both are one-time, manual steps on
https://aur.archlinux.org, since there's no API to automate creating an
account or a new package. Once that's done, add the account's SSH private
key as the `AUR_SSH_PRIVATE_KEY` secret on this repo. Until that secret
exists, the workflow fails at the "Push to AUR" step with a message saying
so, rather than silently doing nothing.

## What's hand-maintained vs. generated

`pkgver`/`pkgrel`/`source`/`sha256sums` in this file are placeholders —
the workflow rewrites them for every real publish and never commits the
result back here, so don't read this file's `pkgver` as "the last published
version." Everything else (`depends`, `pkgdesc`, `package()`) is meant to be
edited by hand normally, the same as any other PKGBUILD.

**A hand-edit made directly in the AUR repo is silently overwritten the
next time this workflow runs.** Every run renders fresh from *this*
repo's template rather than starting from whatever AUR's copy currently
looks like, so a quick fix pushed straight to AUR (bumping `pkgrel` for a
packaging-only issue, say) survives only until the next dispatch. Make
the fix here instead.
