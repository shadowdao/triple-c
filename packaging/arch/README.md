# Arch / CachyOS package

`PKGBUILD` here is the `triple-c-bin` package's template — see triple-c#34
(the "I would like to also have an Arch/CachyOS native version" part of it).
It's written to AUR conventions (and may go there eventually — see
"Publishing" below) but isn't published to the AUR yet.

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

`.gitea/workflows/publish-arch-package.yml` does the actual work: given a
version (or "latest" if none is given), it finds that release's real Linux
asset on GitHub, downloads it, computes real checksums, renders this
template into a version-specific PKGBUILD, validates it with `makepkg` and
`namcap` inside a real Arch container, and attaches the resulting
`.pkg.tar.zst` to that same GitHub release as a downloadable asset —
installable by hand with `sudo pacman -U`.

It is `workflow_dispatch`-only, deliberately — see the workflow file's own
header comment for why an automatic trigger isn't safe here (the same reason
`sync-release.yml` didn't work and was removed in triple-c#32).

**Not on the AUR yet.** Publishing there would need a maintainer AUR account
and its SSH key added as a secret on this repo — both manual, one-time steps
on https://aur.archlinux.org that only a maintainer can do. The workflow's
git history still has the AUR-push step from before this was descoped, if
that setup happens later and it's worth reinstating.

## What's hand-maintained vs. generated

`pkgver`/`pkgrel`/`source`/`sha256sums` in this file are placeholders —
the workflow rewrites them for every real publish and never commits the
result back here, so don't read this file's `pkgver` as "the last published
version." Everything else (`depends`, `pkgdesc`, `package()`) is meant to be
edited by hand normally, the same as any other PKGBUILD.

**A hand-edit made to the rendered PKGBUILD attached to a GitHub release is
not this file.** Every run renders fresh from *this* repo's template, so a
packaging fix belongs here, not in a downloaded copy — the next dispatch for
that version would just overwrite it anyway.
