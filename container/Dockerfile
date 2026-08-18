FROM ubuntu:24.04

# Multi-arch: builds for linux/amd64 and linux/arm64 (Apple Silicon)
# Avoid interactive prompts during package install
ENV DEBIAN_FRONTEND=noninteractive

# ── System packages ──────────────────────────────────────────────────────────
# The shell retry loop handles transient mirror-sync failures where
# archive.ubuntu.com returns stale Packages.gz files with mismatched hashes
# during hourly resyncs. Clearing /var/lib/apt/lists/* between attempts
# forces a fresh fetch.
RUN for i in 1 2 3 4 5; do \
        apt-get -o Acquire::Retries=3 update && break; \
        echo "apt-get update failed (attempt $i), retrying in 10s..."; \
        rm -rf /var/lib/apt/lists/*; \
        sleep 10; \
    done \
    && apt-get install -y --no-install-recommends \
    git \
    curl \
    wget \
    openssh-client \
    build-essential \
    ripgrep \
    jq \
    sudo \
    ca-certificates \
    libnss3-tools \
    gnupg \
    locales \
    unzip \
    pkg-config \
    libssl-dev \
    cron \
    bubblewrap \
    socat \
    iproute2 \
    wireguard-tools \
    iptables \
    && rm -rf /var/lib/apt/lists/*

# `libnss3-tools` above provides `certutil`. Chrome/Chromium read neither
# /etc/ssl/certs nor $SSL_CERT_FILE — they have their own NSS database at
# ~/.pki/nssdb — so without it the browser-view pane cannot be made to trust a
# corporate CA, no matter what the system trust store says. entrypoint.sh
# degrades to a warning if it is ever missing.

# `iproute2`, `wireguard-tools` and `iptables` above are what the VPN support
# toggle (`vpn_support_enabled`) grants capability *for*. That toggle hands a
# project CAP_NET_ADMIN and /dev/net/tun; without `ip` there is then no way to
# add a route, and without `wg` no way to build the tunnel those two exist to
# serve — a capability with nothing able to use it.
#
# They are baked rather than left to a runtime `apt-get install` for the same
# reason as the Playwright libraries below: the writable layer is re-paid after
# every Reset and lost on base-image migration. A hand-installed `wg` therefore
# works right up until an upgrade, then disappears and takes the tunnel with it
# — silently, since a VPN that fails to come up looks exactly like one that was
# never started.
#
# Measured against the *current base image*, since a bare ubuntu:24.04 also
# pulls libelf1t64 and netbase, which this base already has, and so over-reports
# by ~258 kB: **+12 packages, 7,203 kB on amd64**. The same set on arm64 is
# ~14.4 MB — the package list is identical on both arches, the binaries are
# simply larger (measured as 14.7 MB on arm64 ubuntu:24.04, less that 258 kB).
#
# ## Why `iptables`, and not `nftables`
#
# `wireguard-tools` declares `Recommends: nftables | iptables`, which the
# `--no-install-recommends` above strips. That is not cosmetic: `wg-quick`'s
# `add_default()` runs whenever a config has `AllowedIPs = 0.0.0.0/0` — i.e.
# every stock full-tunnel config every provider hands out — and it shells out to
# a firewall backend with no `type -p` guard. Measured with neither installed:
#
#     [#] iptables-restore -n
#     /usr/bin/wg-quick: line 32: iptables-restore: command not found
#     wg-quick EXIT=127
#
# `nftables` looks like the better pick — wg-quick prefers it, it is first in
# that Recommends, it is half the size — and it is the wrong one. wg-quick picks
# nft *unconditionally* when present (`if type -p nft`, line 241), so installing
# it makes the iptables path unreachable; and its nft ruleset needs three
# expression families where the iptables path needs one. Isolating them on a
# WSL2 host, the two connmark rules install fine and this is what fails:
#
#     nft add rule ... fib saddr type != local drop
#     Error: Could not process rule: No such file or directory
#                                   ^^^^^^^^^^^^^^  needs nft_fib_ipv4
#
# The choice therefore turns on which kernel symbol each path needs, and the two
# are not equally safe to bet on. `xt_CONNMARK` (iptables) was present in every
# kernel config examined — LinuxKit's for both arches, and WSL2's from 6.6.
# `nft_fib_ipv4` (nftables) was absent from the LinuxKit config read here, and a
# later review argued Docker Desktop has since enabled it and no longer builds
# from that config at all. That may well be true; it could not be settled from a
# Linux host, and it is the point: nftables' viability varies by Docker Desktop
# version in a way nobody here can pin down, while iptables' requirement did not
# vary anywhere it was checked.
#
# So `iptables` is chosen for being robust to that uncertainty rather than for
# beating nftables on any particular host. If nft_fib_ipv4 is present, wg-quick
# never reaches the iptables path and this costs 1.6 MB and nothing else; if it
# is absent, this is the difference between a working full tunnel and none.
#
# The residual gap is WSL2 before 6.6, which has neither symbol. Nothing
# installable in the container changes that — but `wsl --update` does, and moves
# the host to a far newer kernel. Add the routes with `ip route` in the meantime;
# that needs no firewall backend on any platform.
#
# ## What this still does not fix
#
# `wireguard-tools` only *Suggests* `openresolv | resolvconf`, so neither is
# installed, and every provider's stock config carries a `DNS =` line. That
# fails in `set_dns()`, *before* the firewall step, so it takes split tunnels
# down too:
#
#     [#] resolvconf -a wg0 -m 0 -x
#     /usr/bin/wg-quick: line 32: resolvconf: command not found
#
# Deliberately not fixed here: `openresolv` has no installation candidate on
# noble, and `resolvconf` resolves only by pulling in systemd-resolved — a
# resolver daemon and systemd units, into a container with no systemd. Strip the
# `DNS =` line and set the resolver another way. Documented in HOW-TO-USE.md.

# Remove default ubuntu user to free UID 1000 for host-user remapping
RUN if id ubuntu >/dev/null 2>&1; then userdel -r ubuntu 2>/dev/null || userdel ubuntu; fi \
    && if getent group ubuntu >/dev/null 2>&1; then groupdel ubuntu 2>/dev/null || true; fi

# Set UTF-8 locale
RUN locale-gen en_US.UTF-8
ENV LANG=en_US.UTF-8
ENV LC_ALL=en_US.UTF-8

# ── GitHub CLI ───────────────────────────────────────────────────────────────
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
    | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg \
    && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
    > /etc/apt/sources.list.d/github-cli.list \
    && for i in 1 2 3 4 5; do \
        apt-get -o Acquire::Retries=3 update && break; \
        echo "apt-get update failed (attempt $i), retrying in 10s..."; \
        rm -rf /var/lib/apt/lists/*; \
        sleep 10; \
    done \
    && apt-get install -y gh \
    && rm -rf /var/lib/apt/lists/*

# ── Node.js LTS (22.x) + pnpm ───────────────────────────────────────────────
# Configure NodeSource repo manually (not via their setup_22.x script, which
# runs an internal apt-get update without retries and silently falls through
# to Ubuntu's default nodejs 18 — missing npm — on mirror-sync failures).
RUN curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key \
    | gpg --dearmor -o /usr/share/keyrings/nodesource.gpg \
    && chmod a+r /usr/share/keyrings/nodesource.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/nodesource.gpg] https://deb.nodesource.com/node_22.x nodistro main" \
    > /etc/apt/sources.list.d/nodesource.list \
    && for i in 1 2 3 4 5; do \
        apt-get -o Acquire::Retries=3 update && break; \
        echo "apt-get update failed (attempt $i), retrying in 10s..."; \
        rm -rf /var/lib/apt/lists/*; \
        sleep 10; \
    done \
    && apt-get install -y nodejs \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g pnpm

# ── Browser runtime libraries (Chromium / Google Chrome) ────────────────────
# Chromium links against a set of shared libraries Ubuntu's base image does not
# ship — libnss3, libgbm1, libatk*, libasound2t64, libcups2t64, libpango,
# libdrm2 and friends. Without them `playwright install chromium` downloads a
# browser that then dies at launch with "Host system is missing dependencies:
# libnss3.so", which reads like a Playwright bug and is not one. Installing
# google-chrome-stable used to look like the fix only because apt pulled these
# in as *its* dependencies.
#
# ## Why baked, and why only the libraries
#
# A runtime `apt-get install` lands in the container's writable layer: it is
# re-paid after every project Reset, and it is *lost* on base-image migration,
# which replays apt from a manifest against the new base. The browsers
# themselves live in ~/.cache/ms-playwright, inside the home volume, and survive
# both — so the runtime approach converges on the worst state, a 400 MB browser
# present with its libraries gone. Baking the libraries and leaving the browsers
# out puts each half where it already persists.
#
# Browser binaries are deliberately NOT baked: they are large, they are
# version-coupled to whatever Playwright the user installs, and the home volume
# already keeps them.
#
# ## Why `install-deps` rather than a hand-written apt list
#
# Playwright names its own dependencies, so the list cannot silently rot. That
# matters more than usual on Ubuntu 24.04, whose 64-bit-time_t transition
# renamed a swathe of these packages (libasound2 → libasound2t64, libatk1.0-0 →
# libatk1.0-0t64, libglib2.0-0 → libglib2.0-0t64, …); a hardcoded list drifts
# into "E: Unable to locate package" build failures, and a list that predates a
# new Chromium dependency drifts into exactly the launch failure this layer
# exists to prevent.
#
# Verified on a real `--platform linux/arm64` build of this file, not assumed:
# it resolves and installs there too (99 packages on both arches), and the
# --dry-run assertion below passes. Worth checking rather than assuming:
# Playwright looks its dependency list up under `<distro><version>-<arch>`, so
# arm64 is a separate lookup that could have missed.
#
# ## What it costs
#
# Measured with this layer applied on top of an otherwise identical image
# (linux/amd64, playwright 1.62.1): **+99 packages, +334 MiB unpacked, +119 MiB
# compressed** — the image goes 2950 → 3284 MiB unpacked, 759 → 878 MiB
# compressed. (`docker history` calls the layer 361 MB, i.e. 344 MiB; the
# difference is tar metadata `du` doesn't count.)
#
# Where it goes, by dpkg Installed-Size:
#   ~213 MiB  libllvm20 + mesa-libgallium + libicu74. Not optional and not
#             avoidable by trimming the list: libgbm1, which Chromium genuinely
#             needs, Depends on mesa-libgallium, which Depends on libllvm20.
#   ~94 MiB   Playwright's `tools` group — xvfb and the CJK/emoji fonts. Kept:
#             the base image ships no fonts at all, so without them every page
#             this feature exists to display renders as tofu, and xvfb is what
#             lets a *headed* browser run in here.
#   the rest  Chromium's own library closure.
#
# An explicit apt list of just `chromium`'s dependencies measures 247 MiB
# installed against install-deps' 341 MiB, so hand-maintaining one would save
# ~94 MiB. Not worth owning the drift; if you disagree, derive the list from
# `install-deps --dry-run chromium` and pin the Playwright version you took it
# from in a comment here.
#
# The retry loop is for the same transient mirror-sync failures the other apt
# layers guard against; install-deps runs its own un-retried `apt-get update`
# internally. `npx --yes` is what makes it non-interactive, and the version it
# resolved is printed so a build log says which Playwright named this set.
#
# Placed immediately after Node (npx is its only prerequisite) and well above
# the shim COPYs, so editing a shim at the bottom of this file does not re-run a
# multi-hundred-megabyte apt install.
#
# `--dry-run` afterwards is the build-time assertion, and it is not decoration:
# on a platform Playwright's table does not cover, `install-deps` prints a
# warning and returns having installed **nothing, with exit status 0**. Without
# this check that failure mode would ship an image whose build log looked clean.
# `--dry-run` exits non-zero if any required package is still missing.
RUN npx --yes playwright@latest --version \
    && ok=0 \
    && for i in 1 2 3 4 5; do \
        if npx --yes playwright@latest install-deps chromium; then ok=1; break; fi; \
        echo "install-deps failed (attempt $i), retrying in 10s..."; \
        rm -rf /var/lib/apt/lists/*; \
        sleep 10; \
    done \
    && [ "$ok" = 1 ] \
    && npx --yes playwright@latest install-deps --dry-run chromium \
    && rm -rf /var/lib/apt/lists/* /root/.npm

# ── Python 3 + pip + uv + ruff ──────────────────────────────────────────────
RUN for i in 1 2 3 4 5; do \
        apt-get -o Acquire::Retries=3 update && break; \
        echo "apt-get update failed (attempt $i), retrying in 10s..."; \
        rm -rf /var/lib/apt/lists/*; \
        sleep 10; \
    done \
    && apt-get install -y --no-install-recommends \
    python3 \
    python3-pip \
    python3-venv \
    && rm -rf /var/lib/apt/lists/*

# ── Docker CLI (not daemon) ─────────────────────────────────────────────────
RUN install -m 0755 -d /etc/apt/keyrings \
    && curl -fsSL https://download.docker.com/linux/ubuntu/gpg \
    | gpg --dearmor -o /etc/apt/keyrings/docker.gpg \
    && chmod a+r /etc/apt/keyrings/docker.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
    > /etc/apt/sources.list.d/docker.list \
    && for i in 1 2 3 4 5; do \
        apt-get -o Acquire::Retries=3 update && break; \
        echo "apt-get update failed (attempt $i), retrying in 10s..."; \
        rm -rf /var/lib/apt/lists/*; \
        sleep 10; \
    done \
    && apt-get install -y docker-ce-cli \
    && rm -rf /var/lib/apt/lists/*

# ── AWS CLI v2 ───────────────────────────────────────────────────────────────
RUN ARCH=$(uname -m) && \
    curl "https://awscli.amazonaws.com/awscli-exe-linux-${ARCH}.zip" -o "awscliv2.zip" && \
    unzip -q awscliv2.zip && \
    ./aws/install && \
    rm -rf awscliv2.zip aws

# ── Non-root user with passwordless sudo ─────────────────────────────────────
RUN useradd -m -s /bin/bash -u 1000 claude \
    && echo "claude ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/claude \
    && chmod 0440 /etc/sudoers.d/claude

# ── Mount points (created as root, owned by claude) ──────────────────────────
RUN mkdir -p /workspace && chown claude:claude /workspace

# ── Rust (installed as claude user) ──────────────────────────────────────────
USER claude
WORKDIR /home/claude

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/home/claude/.cargo/bin:${PATH}"

# Install uv and ruff for claude user
RUN curl -LsSf https://astral.sh/uv/install.sh | sh \
    && curl -LsSf https://astral.sh/ruff/install.sh | sh
ENV PATH="/home/claude/.local/bin:/home/claude/.cargo/bin:${PATH}"

# ── Claude Code ──────────────────────────────────────────────────────────────
RUN curl -fsSL https://claude.ai/install.sh | bash
ENV PATH="/home/claude/.claude/bin:${PATH}"

RUN mkdir -p /home/claude/.claude /home/claude/.ssh

WORKDIR /workspace

# ── Switch back to root for entrypoint (handles UID/GID remapping) ─────────
USER root

# ── OSC 52 clipboard support ─────────────────────────────────────────────
# Provides xclip/xsel/pbcopy shims that emit OSC 52 escape sequences,
# allowing programs inside the container to copy to the host clipboard.
COPY osc52-clipboard /usr/local/bin/osc52-clipboard
RUN chmod +x /usr/local/bin/osc52-clipboard \
    && ln -sf /usr/local/bin/osc52-clipboard /usr/local/bin/xclip \
    && ln -sf /usr/local/bin/osc52-clipboard /usr/local/bin/xsel \
    && ln -sf /usr/local/bin/osc52-clipboard /usr/local/bin/pbcopy

# ── Audio capture shim (voice mode) ────────────────────────────────────────
# Provides fake rec/arecord that read PCM from a FIFO instead of a real mic,
# allowing Claude Code voice mode to work inside the container.
COPY audio-shim /usr/local/bin/audio-shim
RUN chmod +x /usr/local/bin/audio-shim \
    && ln -sf /usr/local/bin/audio-shim /usr/local/bin/rec \
    && ln -sf /usr/local/bin/audio-shim /usr/local/bin/arecord

# ── URL relay shim (host browser) ───────────────────────────────────────────
# Container-side stand-in for a browser. Emits an OSC 7777 escape sequence that
# Triple-C's terminal front-end intercepts and turns into a host-browser open.
# Installed under every name a CLI conventionally consults, plus $BROWSER.
#
# What Ubuntu 24.04's base actually ships (verified, not assumed):
#   sensible-browser  PRESENT  (/usr/bin/sensible-browser, from sensible-utils)
#   xdg-open          absent   (xdg-utils is not installed)
#   www-browser       absent   (no update-alternatives entry)
#   x-www-browser     absent   (no update-alternatives entry)
#   gnome-open / gvfs-open / kde-open / open   absent
#
# So the three names that need real handling, not just a symlink:
#   * sensible-browser is a dpkg-owned file. A /usr/local/bin symlink would
#     only shadow it for PATH lookups, leaving absolute-path callers on the
#     stock script — so it is dpkg-diverted and replaced. (The stock script
#     does defer to $BROWSER, but only when $BROWSER is set; diverting makes
#     the behaviour unconditional and survives package upgrades.)
#   * www-browser / x-www-browser are update-alternatives names, so they are
#     registered as alternatives rather than hand-symlinked. This matters:
#     sensible-browser probes /usr/bin/x-www-browser by absolute path, which
#     only exists if something registered the alternative. `--set` pins them
#     to manual mode so a later `apt install firefox` cannot steal them and
#     point the container at a browser it has no display to run.
#   * xdg-open is diverted pre-emptively so that if someone later installs
#     xdg-utils inside the container, dpkg unpacks to xdg-open.distrib and
#     our relay keeps /usr/bin/xdg-open.
COPY triple-c-open /usr/local/bin/triple-c-open
RUN chmod +x /usr/local/bin/triple-c-open \
    && for name in xdg-open sensible-browser gnome-open gvfs-open kde-open open; do \
           ln -sf /usr/local/bin/triple-c-open "/usr/local/bin/$name"; \
       done \
    && dpkg-divert --local --rename --divert /usr/bin/sensible-browser.distrib \
           --add /usr/bin/sensible-browser \
    && ln -sf /usr/local/bin/triple-c-open /usr/bin/sensible-browser \
    && dpkg-divert --local --rename --divert /usr/bin/xdg-open.distrib \
           --add /usr/bin/xdg-open \
    && ln -sf /usr/local/bin/triple-c-open /usr/bin/xdg-open \
    && update-alternatives --install /usr/bin/x-www-browser x-www-browser \
           /usr/local/bin/triple-c-open 200 \
    && update-alternatives --set x-www-browser /usr/local/bin/triple-c-open \
    && update-alternatives --install /usr/bin/www-browser www-browser \
           /usr/local/bin/triple-c-open 200 \
    && update-alternatives --set www-browser /usr/local/bin/triple-c-open

# $BROWSER must be an image-level ENV, not just an entrypoint export: terminal
# sessions are separate `docker exec`s, which inherit the container's config
# env and see nothing the entrypoint exported into its own process. The
# entrypoint additionally forwards it into the cron environment file.
ENV BROWSER=/usr/local/bin/triple-c-open

COPY triple-c-sso-refresh /usr/local/bin/triple-c-sso-refresh
RUN chmod +x /usr/local/bin/triple-c-sso-refresh

COPY mission-control /opt/mission-control

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
COPY triple-c-scheduler /usr/local/bin/triple-c-scheduler
RUN chmod +x /usr/local/bin/triple-c-scheduler
# Lives in /usr/local/bin rather than under /home/claude on purpose: the home
# directory is the mount point of the project's home volume, so an image copy
# of it is masked after the project's first start and can never be updated
# again. /usr/local/bin rides the snapshot and is replaced on migration, which
# is what lets a fix to this script reach an existing project at all.
COPY triple-c-playwright-heal /usr/local/bin/triple-c-playwright-heal
RUN chmod +x /usr/local/bin/triple-c-playwright-heal
COPY triple-c-task-runner /usr/local/bin/triple-c-task-runner
RUN chmod +x /usr/local/bin/triple-c-task-runner

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
