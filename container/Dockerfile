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
    && rm -rf /var/lib/apt/lists/*

# `libnss3-tools` above provides `certutil`. Chrome/Chromium read neither
# /etc/ssl/certs nor $SSL_CERT_FILE — they have their own NSS database at
# ~/.pki/nssdb — so without it the browser-view pane cannot be made to trust a
# corporate CA, no matter what the system trust store says. entrypoint.sh
# degrades to a warning if it is ever missing.

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
COPY triple-c-task-runner /usr/local/bin/triple-c-task-runner
RUN chmod +x /usr/local/bin/triple-c-task-runner

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
