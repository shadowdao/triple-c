#!/bin/bash
# NOTE: set -e is intentionally omitted. A failing usermod/groupmod must not
# kill the entire entrypoint — SSH setup, git config, and the final exec
# must still run so the container is usable even if remapping fails.
#
# NOTE: /home/claude is the mount point of the named volume
# triple-c-home-{projectId}, so the *image's* copy of that directory is
# seed-only: after a project's first start it is masked permanently. Anything
# this script writes under /home/claude on **every** start does reach existing
# projects (that is why the CLAUDE.md, git config and Mission Control skill
# copies are written here rather than baked into the image). Anything added to
# /home/claude in the Dockerfile reaches new projects only, forever. Put
# upgradable content in /usr/local/bin or /opt, or seed it from here.
# See "Container Lifecycle" in the repo's CLAUDE.md.

# ── UID/GID remapping ──────────────────────────────────────────────────────
# Match the container's claude user to the host user's UID/GID so that
# bind-mounted files (project dir, docker socket) have correct ownership.
remap_uid_gid() {
    local target_uid="${HOST_UID}"
    local target_gid="${HOST_GID}"
    local current_uid
    local current_gid
    current_uid=$(id -u claude 2>/dev/null) || { echo "entrypoint: claude user not found"; return 1; }
    current_gid=$(id -g claude 2>/dev/null) || { echo "entrypoint: claude group not found"; return 1; }

    # ── GID remapping ──
    if [ -n "$target_gid" ] && [ "$target_gid" != "$current_gid" ]; then
        # If another group already holds the target GID, move it out of the way
        local blocking_group
        blocking_group=$(getent group "$target_gid" 2>/dev/null | cut -d: -f1)
        if [ -n "$blocking_group" ] && [ "$blocking_group" != "claude" ]; then
            echo "entrypoint: moving group '$blocking_group' from GID $target_gid to 65533"
            groupmod -g 65533 "$blocking_group" || echo "entrypoint: warning — failed to relocate group '$blocking_group'"
        fi
        groupmod -g "$target_gid" claude \
            && echo "entrypoint: claude GID -> $target_gid" \
            || echo "entrypoint: warning — groupmod -g $target_gid claude failed"
    fi

    # ── UID remapping ──
    if [ -n "$target_uid" ] && [ "$target_uid" != "$current_uid" ]; then
        # If another user already holds the target UID, move it out of the way
        local blocking_user
        blocking_user=$(getent passwd "$target_uid" 2>/dev/null | cut -d: -f1)
        if [ -n "$blocking_user" ] && [ "$blocking_user" != "claude" ]; then
            echo "entrypoint: moving user '$blocking_user' from UID $target_uid to 65533"
            usermod -u 65533 "$blocking_user" || echo "entrypoint: warning — failed to relocate user '$blocking_user'"
        fi
        usermod -u "$target_uid" claude \
            && echo "entrypoint: claude UID -> $target_uid" \
            || echo "entrypoint: warning — usermod -u $target_uid claude failed"
    fi
}

remap_uid_gid

# Fix ownership of home directory after UID/GID change
chown -R claude:claude /home/claude

# ── Corporate CA certificates ───────────────────────────────────────────────
# The host's CA material is bind-mounted read-only at /tmp/.host-ca. Triple-C
# mounts a *directory* as-is and a *single file* as /tmp/.host-ca/<name>.crt,
# so this only ever has to deal with a directory (the file branch below is
# defensive).
#
# Runs before everything that touches the network — the git credential helper,
# ssh-keyscan, and especially the `claude update` at the bottom of this file,
# which is itself an HTTPS call that fails behind a TLS-terminating proxy
# without this.
#
# Two things are easy to get wrong here:
#   1. `update-ca-certificates` globs /usr/local/share/ca-certificates/*.crt
#      case-sensitively. A `.pem` that is merely copied in is ignored in total
#      silence, so certificates are *renamed*, not copied.
#   2. Chrome/Chromium read neither /etc/ssl nor $SSL_CERT_FILE; they have
#      their own NSS database at ~/.pki/nssdb, seeded below with certutil.
#
# NODE_EXTRA_CA_CERTS / REQUESTS_CA_BUNDLE / SSL_CERT_FILE are deliberately NOT
# exported here. Every terminal is a separate `docker exec`, which inherits the
# container's configured env and sees nothing this script exported — the same
# reason $BROWSER had to become an image-level ENV. Triple-C sets them on the
# container at creation time instead. (They are forwarded into the cron
# environment file further down, because cron jobs start from a bare env.)
CA_SRC="/tmp/.host-ca"
CA_STORE="/usr/local/share/ca-certificates"
CA_PREFIX="triple-c-"
CA_BUNDLE="/etc/ssl/certs/ca-certificates.crt"
CA_STAMP="/var/lib/triple-c/ca.stamp"
CA_NSSDB="/home/claude/.pki/nssdb"

# Mirror of `container_cert_name()` in app/src-tauri/src/docker/ca_certs.rs.
# The two must agree; the Rust side has the unit tests.
ca_normalise_name() {
    local name stem
    name=$(printf '%s' "$1" | tr -c 'A-Za-z0-9._-' '_')
    while [ "${name#.}" != "$name" ]; do name="${name#.}"; done
    stem="${name%.*}"
    [ -z "$stem" ] && stem="corporate-ca"
    printf '%s.crt' "$stem"
}

ca_source_files() {
    if [ -d "$CA_SRC" ]; then
        find "$CA_SRC" -maxdepth 1 -type f \
            \( -iname '*.crt' -o -iname '*.pem' -o -iname '*.cer' \
               -o -iname '*.cert' -o -iname '*.ca-bundle' \) 2>/dev/null | sort
    elif [ -f "$CA_SRC" ]; then
        printf '%s\n' "$CA_SRC"
    fi
}

# Seed Chrome/Chromium's NSS database. Tolerant by design: a missing certutil
# or a broken profile must warn, never fail the container start.
# ~/.pki lives in the home volume, so this persists once done; the system store
# lives in the writable layer and is re-applied on every start.
ca_seed_nssdb() {
    if ! command -v certutil >/dev/null 2>&1; then
        echo "entrypoint: warning — certutil not found (install libnss3-tools); Chrome/Chromium in this container will not trust the corporate CA"
        return 0
    fi
    su -s /bin/bash claude -c '
        db="$HOME/.pki/nssdb"
        mkdir -p "$db" || exit 1
        if [ ! -f "$db/cert9.db" ]; then
            certutil -d "sql:$db" -N --empty-password >/dev/null 2>&1 || exit 1
        fi
        for f in /usr/local/share/ca-certificates/triple-c-*.crt; do
            [ -f "$f" ] || continue
            nick="triple-c:$(basename "$f" .crt)"
            # Delete first so re-running replaces rather than duplicates.
            certutil -d "sql:$db" -D -n "$nick" >/dev/null 2>&1
            certutil -d "sql:$db" -A -t "C,," -n "$nick" -i "$f" >/dev/null 2>&1 \
                || echo "entrypoint: warning — certutil could not add $nick"
        done
    ' && echo "entrypoint: seeded Chrome/Chromium NSS database with the corporate CA" \
      || echo "entrypoint: warning — NSS database seeding failed (continuing)"
}

install_corporate_ca() {
    local files fp stamp f base name count installed

    files=$(ca_source_files)

    if [ -z "$files" ]; then
        # Nothing configured — but /usr/local/share is in the writable layer and
        # `docker commit` bakes it into the project's snapshot image, so a cert
        # installed by a previous configuration would ride that snapshot into
        # every future container. Turning the setting off has to actively undo.
        if ls "$CA_STORE/$CA_PREFIX"*.crt >/dev/null 2>&1; then
            echo "entrypoint: removing previously installed corporate CA certificates"
            rm -f "$CA_STORE/$CA_PREFIX"*.crt
            update-ca-certificates --fresh >/dev/null 2>&1 \
                || echo "entrypoint: warning — update-ca-certificates failed while removing certificates"
            rm -f "$CA_STAMP"
        fi
        if [ -e "$CA_SRC" ]; then
            echo "entrypoint: warning — $CA_SRC holds no certificate files"
        fi
        return 0
    fi

    # Idempotent and cheap: the certs are already installed on a plain restart
    # (the writable layer survives stop/start), so hash the sources and skip the
    # work when nothing has moved. The NSS database is checked separately
    # because it lives in the home volume and can be wiped independently.
    fp=$(printf '%s\n' "$files" | xargs -d '\n' -r sha256sum 2>/dev/null | sha256sum | cut -d' ' -f1)
    stamp=$(cat "$CA_STAMP" 2>/dev/null)
    if [ -n "$fp" ] && [ "$fp" = "$stamp" ] && [ -s "$CA_BUNDLE" ]; then
        if [ -f "$CA_NSSDB/cert9.db" ]; then
            echo "entrypoint: corporate CA certificates already installed"
            return 0
        fi
        ca_seed_nssdb
        return 0
    fi

    mkdir -p "$CA_STORE" "$(dirname "$CA_STAMP")"
    rm -f "$CA_STORE/$CA_PREFIX"*.crt
    installed=0

    while IFS= read -r f; do
        [ -n "$f" ] || continue
        base=$(basename "$f")
        name="$CA_PREFIX$(ca_normalise_name "$base")"
        count=$(grep -c -- '-----BEGIN CERTIFICATE-----' "$f" 2>/dev/null || true)
        [ -n "$count" ] || count=0
        if [ "$count" -gt 1 ]; then
            # A corporate trust chain is usually delivered as one PEM holding
            # root + intermediates. update-ca-certificates handles exactly one
            # certificate per file, so split it.
            awk -v out="$CA_STORE/${name%.crt}" '
                /-----BEGIN CERTIFICATE-----/ { n++; f = out "-" n ".crt" }
                n > 0 { print > f }
            ' "$f" && installed=$((installed + count))
        elif [ "$count" -eq 1 ]; then
            cp -f "$f" "$CA_STORE/$name" && installed=$((installed + 1))
        else
            echo "entrypoint: warning — $f holds no PEM certificate (DER is not supported), skipping"
        fi
    done <<< "$files"

    chmod 644 "$CA_STORE/$CA_PREFIX"*.crt 2>/dev/null

    if [ "$installed" -eq 0 ]; then
        echo "entrypoint: warning — no usable certificates found under $CA_SRC"
        return 0
    fi

    if update-ca-certificates >/dev/null 2>&1; then
        echo "entrypoint: installed $installed corporate CA certificate(s) into the system trust store"
        printf '%s' "$fp" > "$CA_STAMP"
    else
        echo "entrypoint: warning — update-ca-certificates failed; corporate certificates may not be trusted"
    fi

    ca_seed_nssdb
}

install_corporate_ca

# ── SSH key setup ──────────────────────────────────────────────────────────
# Host SSH dir is mounted read-only at /tmp/.host-ssh.
# Copy to /home/claude/.ssh so we can fix permissions.
if [ -d /tmp/.host-ssh ]; then
    rm -rf /home/claude/.ssh
    cp -a /tmp/.host-ssh /home/claude/.ssh
    chown -R claude:claude /home/claude/.ssh
    chmod 700 /home/claude/.ssh
    find /home/claude/.ssh -type f -name "id_*" ! -name "*.pub" -exec chmod 600 {} \;
    find /home/claude/.ssh -type f -name "*.pub" -exec chmod 644 {} \;
    if [ -f /home/claude/.ssh/known_hosts ]; then
        chmod 644 /home/claude/.ssh/known_hosts
    fi
    if [ -f /home/claude/.ssh/config ]; then
        chmod 600 /home/claude/.ssh/config
    fi
fi

# Append common host keys (avoid duplicates)
su -s /bin/bash claude -c '
    mkdir -p /home/claude/.ssh
    ssh-keyscan -t ed25519,rsa github.com gitlab.com bitbucket.org >> /home/claude/.ssh/known_hosts 2>/dev/null || true
    sort -u -o /home/claude/.ssh/known_hosts /home/claude/.ssh/known_hosts
'

# ── AWS config setup ──────────────────────────────────────────────────────────
# Host AWS dir is mounted read-only at /tmp/.host-aws.
# Copy to /home/claude/.aws so AWS CLI can write to sso/cache and cli/cache.
if [ -d /tmp/.host-aws ]; then
    rm -rf /home/claude/.aws
    cp -a /tmp/.host-aws /home/claude/.aws
    chown -R claude:claude /home/claude/.aws
    chmod 700 /home/claude/.aws
    # Ensure writable cache directories exist
    mkdir -p /home/claude/.aws/sso/cache /home/claude/.aws/cli/cache
    chown -R claude:claude /home/claude/.aws/sso /home/claude/.aws/cli

    # Inline sso_session properties into profile sections so AWS SDKs that don't
    # support the sso_session indirection format can resolve sso_region, etc.
    if [ -f /home/claude/.aws/config ]; then
        python3 -c '
import configparser, sys
c = configparser.ConfigParser()
c.read(sys.argv[1])
for sec in c.sections():
    if not sec.startswith("profile ") and sec != "default":
        continue
    session = c.get(sec, "sso_session", fallback=None)
    if not session or c.has_option(sec, "sso_start_url"):
        continue
    ss = f"sso-session {session}"
    if not c.has_section(ss):
        continue
    for key in ("sso_start_url", "sso_region", "sso_registration_scopes"):
        val = c.get(ss, key, fallback=None)
        if val:
            c.set(sec, key, val)
with open(sys.argv[1], "w") as f:
    c.write(f)
' /home/claude/.aws/config 2>/dev/null || true
    fi
fi

# ── Git credential helper (for HTTPS token) ─────────────────────────────────
if [ -n "$GIT_TOKEN" ]; then
    CRED_FILE="/home/claude/.git-credentials"
    : > "$CRED_FILE"
    chmod 600 "$CRED_FILE"
    chown claude:claude "$CRED_FILE"
    echo "https://oauth2:${GIT_TOKEN}@github.com" >> "$CRED_FILE"
    echo "https://oauth2:${GIT_TOKEN}@gitlab.com" >> "$CRED_FILE"
    echo "https://oauth2:${GIT_TOKEN}@bitbucket.org" >> "$CRED_FILE"
    git config --file /home/claude/.gitconfig credential.helper "store --file=$CRED_FILE"
    unset GIT_TOKEN
fi

# ── Git user config ──────────────────────────────────────────────────────────
if [ -n "$GIT_USER_NAME" ]; then
    git config --file /home/claude/.gitconfig user.name "$GIT_USER_NAME"
fi
if [ -n "$GIT_USER_EMAIL" ]; then
    git config --file /home/claude/.gitconfig user.email "$GIT_USER_EMAIL"
fi
chown claude:claude /home/claude/.gitconfig 2>/dev/null || true

# ── Claude instructions ──────────────────────────────────────────────────────
if [ -n "$CLAUDE_INSTRUCTIONS" ]; then
    mkdir -p /home/claude/.claude
    printf '%s\n' "$CLAUDE_INSTRUCTIONS" > /home/claude/.claude/CLAUDE.md
    chown claude:claude /home/claude/.claude/CLAUDE.md
    unset CLAUDE_INSTRUCTIONS
fi

# ── Mission Control setup ───────────────────────────────────────────────────
if [ "$MISSION_CONTROL_ENABLED" = "1" ]; then
    MC_HOME="/home/claude/mission-control"
    MC_LINK="/workspace/mission-control"
    if [ ! -d "$MC_HOME" ]; then
        echo "entrypoint: installing mission-control..."
        cp -r /opt/mission-control "$MC_HOME"
        chown -R claude:claude "$MC_HOME"
    else
        echo "entrypoint: mission-control already present, skipping install"
    fi
    # Symlink into workspace so Claude sees it at /workspace/mission-control
    ln -sfn "$MC_HOME" "$MC_LINK"
    chown -h claude:claude "$MC_LINK"

    # Install skills to ~/.claude/skills/ so Claude Code discovers them automatically
    if [ -d "$MC_HOME/.claude/skills" ]; then
        mkdir -p /home/claude/.claude/skills
        cp -r "$MC_HOME/.claude/skills/"* /home/claude/.claude/skills/ 2>/dev/null
        chown -R claude:claude /home/claude/.claude/skills
        echo "entrypoint: mission-control skills installed to ~/.claude/skills/"
    fi

    unset MISSION_CONTROL_ENABLED
fi

# ── Feature skills ──────────────────────────────────────────────────────────
# Skills owned by a Triple-C feature rather than by Mission Control. Installed
# when the feature is on, removed when it is off: ~/.claude is a persisted
# volume, so a skill left behind after its feature is disabled would keep
# telling an agent to use a capability the container no longer has.
#
# Copied on every start rather than only when absent, so a fix to a skill
# reaches projects that already have the old copy. Local edits under these
# directories do not survive — treat /opt/triple-c-skills as the source.
#
# The source lives in the *base image*, so a project whose container predates it
# recreates from its own snapshot and has no /opt/triple-c-skills to copy from.
# That case says so rather than returning silently: the toggle is on, the
# capability is there, and the skill simply never appears — which is impossible
# to work out from the outside.
install_feature_skill() {
    local _name="$1"
    local _enabled="$2"
    local _src="/opt/triple-c-skills/$1"
    local _dest="/home/claude/.claude/skills/$1"

    # Reject anything that is not a plain directory name. The disabled branch
    # `rm -rf`s $_dest under a *persisted volume*, so a blank name would take the
    # whole skills directory (Mission Control's included) and `../x` would escape
    # it entirely. Only the literal `pia-vpn` is passed today; this is so that
    # stays true.
    case "$_name" in
        ''|*/*|.*) echo "entrypoint: install_feature_skill: bad skill name '$_name'"; return 1 ;;
    esac

    if [ "$_enabled" = "1" ]; then
        if [ ! -d "$_src" ]; then
            echo "entrypoint: $_name skill unavailable — this container's base image predates it; migrate the project to get it"
            return 0
        fi
        # Checked, not assumed: with no `set -e` in this script every step here
        # can fail (full volume, read-only mount, a file where the directory
        # should be) and the success line would still print.
        mkdir -p /home/claude/.claude/skills || {
            echo "entrypoint: $_name skill install FAILED (cannot create ~/.claude/skills)"; return 1; }
        # Not just $_dest: when Mission Control is off nothing else creates the
        # parent, so root would own it and `claude` could not add a skill there.
        chown claude:claude /home/claude/.claude/skills
        # Stage then swap. Copying over the live path meant a failure (full
        # volume, read-only mount) left a truncated SKILL.md and no script
        # behind, root-owned, on a persisted volume — which Claude Code then
        # discovers and loads.
        rm -rf "$_dest.new"
        cp -r "$_src" "$_dest.new" || {
            rm -rf "$_dest.new"
            echo "entrypoint: $_name skill install FAILED (copy from $_src); previous copy left intact"
            return 1; }
        chown -R claude:claude "$_dest.new"
        rm -rf "$_dest"
        mv "$_dest.new" "$_dest"
        echo "entrypoint: $_name skill installed to ~/.claude/skills/"
    elif [ -e "$_dest" ] || [ -L "$_dest" ]; then
        # -e/-L rather than -d: a leftover *file* at that path must go too.
        rm -rf "$_dest"
        echo "entrypoint: $_name skill removed (feature disabled)"
    fi
}

install_feature_skill pia-vpn "${VPN_SUPPORT_ENABLED:-0}"
unset VPN_SUPPORT_ENABLED

# ── Claude Code settings ────────────────────────────────────────────────────
# Apply the managed Claude Code settings to ~/.claude/settings.json, keeping
# every key the user set inside the container.
#
# `settings.json` lives on the persisted triple-c-claude-config-{id} volume, so
# it outlives the container and a plain `.[0] * .[1]` merge could only ever
# *add*. That is what made every one of these settings one-way: switching one
# off in Triple-C omitted its key, the merge preserved the old on-value, and the
# setting stayed on until a destructive Reset. So the payload from Rust states
# the whole managed key set on every start, and a JSON **null** in it means
# "delete this key" rather than "merge a null" — which is how a setting whose
# neutral state is *unset* (`tui`, `effortLevel`, `viewMode`,
# `awaySummaryEnabled`) is turned back off without pinning a stand-in value.
# See `build_claude_code_settings_json` in app/src-tauri/src/docker/container.rs.
if [ -n "$CLAUDE_CODE_SETTINGS_JSON" ]; then
    SETTINGS_FILE="/home/claude/.claude/settings.json"
    mkdir -p /home/claude/.claude
    # One code path for "file exists" and "file doesn't": seeding an empty
    # object means the null-deleting merge below runs in both cases, so a fresh
    # container never gets a settings.json with literal nulls written into it.
    [ -f "$SETTINGS_FILE" ] || printf '{}\n' > "$SETTINGS_FILE"
    MERGED=$(jq -s '
        .[0] as $current
        | .[1] as $managed
        | ($managed | with_entries(select(.value != null)))          as $set
        | ($managed | to_entries | map(select(.value == null) | [.key])) as $clear
        | ($current * $set) | delpaths($clear)
    ' "$SETTINGS_FILE" <(printf '%s' "$CLAUDE_CODE_SETTINGS_JSON") 2>/dev/null)
    if [ -n "$MERGED" ]; then
        printf '%s\n' "$MERGED" > "$SETTINGS_FILE"
    else
        echo "entrypoint: warning — failed to merge Claude Code settings into $SETTINGS_FILE"
    fi
    chown claude:claude "$SETTINGS_FILE"
    chmod 600 "$SETTINGS_FILE"
    unset CLAUDE_CODE_SETTINGS_JSON
fi

# ── AWS SSO auth refresh command ──────────────────────────────────────────────
# When set (Bedrock + profile/SSO auth), inject awsAuthRefresh into
# ~/.claude.json so Claude Code calls triple-c-sso-refresh when AWS credentials
# expire mid-session. When NOT set, strip any awsAuthRefresh left behind by a
# previous Bedrock-profile session — ~/.claude.json lives in the persisted home
# volume, so without this the container keeps trying to run the SSO refresh even
# after switching to a non-SSO backend (Anthropic/Ollama) or to static creds.
CLAUDE_JSON="/home/claude/.claude.json"
if [ -n "$AWS_SSO_AUTH_REFRESH_CMD" ]; then
    if [ -f "$CLAUDE_JSON" ]; then
        MERGED=$(jq --arg cmd "$AWS_SSO_AUTH_REFRESH_CMD" '.awsAuthRefresh = $cmd' "$CLAUDE_JSON" 2>/dev/null)
        if [ -n "$MERGED" ]; then
            printf '%s\n' "$MERGED" > "$CLAUDE_JSON"
        fi
    else
        printf '{"awsAuthRefresh":"%s"}\n' "$AWS_SSO_AUTH_REFRESH_CMD" > "$CLAUDE_JSON"
    fi
    chown claude:claude "$CLAUDE_JSON"
    chmod 600 "$CLAUDE_JSON"
    unset AWS_SSO_AUTH_REFRESH_CMD
elif [ -f "$CLAUDE_JSON" ] && grep -q '"awsAuthRefresh"' "$CLAUDE_JSON" 2>/dev/null; then
    # Only rewrite when the key is actually present, to avoid a needless jq
    # reformat of ~/.claude.json on every start of a non-SSO backend.
    MERGED=$(jq 'del(.awsAuthRefresh)' "$CLAUDE_JSON" 2>/dev/null)
    if [ -n "$MERGED" ]; then
        printf '%s\n' "$MERGED" > "$CLAUDE_JSON"
        chown claude:claude "$CLAUDE_JSON"
        chmod 600 "$CLAUDE_JSON"
    fi
fi

# ── Docker socket permissions ────────────────────────────────────────────────
if [ -S /var/run/docker.sock ]; then
    DOCKER_GID=$(stat -c '%g' /var/run/docker.sock)
    if ! getent group "$DOCKER_GID" > /dev/null 2>&1; then
        groupadd -g "$DOCKER_GID" docker-host
    fi
    DOCKER_GROUP=$(getent group "$DOCKER_GID" | cut -d: -f1)
    usermod -aG "$DOCKER_GROUP" claude
fi

# ── Timezone setup ───────────────────────────────────────────────────────────
if [ -n "${TZ:-}" ]; then
    if [ -f "/usr/share/zoneinfo/$TZ" ]; then
        ln -sf "/usr/share/zoneinfo/$TZ" /etc/localtime
        echo "$TZ" > /etc/timezone
        echo "entrypoint: timezone set to $TZ"
    else
        echo "entrypoint: warning — timezone '$TZ' not found in /usr/share/zoneinfo"
    fi
fi

# ── Browser / URL relay ──────────────────────────────────────────────────────
# Tools that open a browser (gh, aws sso login, gcloud, python webbrowser, ...)
# consult $BROWSER first. Point it at the relay shim, which forwards the URL to
# the host's browser over the terminal. The image already sets this as an ENV —
# that is what `docker exec` terminal sessions inherit — but exporting it here
# means the entrypoint's own children see it too, and (below) that it is
# captured into the cron environment file for scheduled tasks. Under cron there
# is no terminal, so the shim degrades to printing the URL into the task log.
if [ -x /usr/local/bin/triple-c-open ]; then
    export BROWSER=/usr/local/bin/triple-c-open
fi

# ── Playwright browser config ───────────────────────────────────────────────
# Seed ~/.playwright/cli.config.json on every start.
#
# Without it `playwright-cli` resolves to channel `chrome` — system Google
# Chrome — with the Chromium sandbox ON, and these containers do not permit
# unprivileged user namespaces, so the browser aborts with "Failed to move to
# new namespace ... Operation not permitted". On a base image that no longer
# ships Google Chrome the same default fails the other way, with "Chromium
# distribution 'chrome' is not found". One cause, two error messages, and
# neither of them looks like a configuration problem.
#
# Seeded here rather than baked into the image because ~/.playwright is inside
# the home volume: an image copy would reach new projects only, and every
# existing project would stay broken forever. Written on every start from a
# source outside the volume, the way CLAUDE_INSTRUCTIONS and the Mission
# Control skills already are.
#
# --seed-config-only is the cheap path: no npm install, no browser download, no
# apt, no verify launch, nothing over the network. It writes one small file if
# it is absent and returns. Measured at ~2 ms. The heavier repairs stay
# on-demand — run `triple-c-playwright-heal` with no arguments for those.
if [ -x /usr/local/bin/triple-c-playwright-heal ]; then
    /usr/local/bin/triple-c-playwright-heal --seed-config-only --quiet || \
        echo "entrypoint: warning — playwright config seeding failed (browser view may not launch)"
fi

# ── Scheduler setup ─────────────────────────────────────────────────────────
SCHEDULER_DIR="/home/claude/.claude/scheduler"
mkdir -p "$SCHEDULER_DIR/tasks" "$SCHEDULER_DIR/logs" "$SCHEDULER_DIR/notifications"
chown -R claude:claude "$SCHEDULER_DIR"

# Start cron daemon (runs as root, executes jobs per user crontab)
cron

# Save environment variables for cron jobs (cron runs with a minimal env)
#
# HOME is deliberately NOT captured here. This entrypoint runs as root, so the
# snapshot would record HOME=/root — and the task runner sources this file with
# `set -a`, which would overwrite the HOME cron gives the job. Claude Code then
# looks for its OAuth credential at /root/.claude/.credentials.json instead of
# /home/claude/.claude/.credentials.json and every scheduled task dies with
# "Not logged in · Please run /login". Cron still needs a HOME, so it is written
# explicitly below with the value the `claude` user actually has.
ENV_FILE="$SCHEDULER_DIR/.env"
: > "$ENV_FILE"
env | while IFS='=' read -r key value; do
    case "$key" in
        ANTHROPIC_*|AWS_*|CLAUDE_CODE_*|TRIPLE_C_PERMISSION_MODE|PATH|LANG|TZ|COLORTERM|BROWSER|NODE_EXTRA_CA_CERTS|REQUESTS_CA_BUNDLE|SSL_CERT_FILE)
            # Escape single quotes in value and write as KEY='VALUE'
            escaped_value=$(printf '%s' "$value" | sed "s/'/'\\\\''/g")
            printf "%s='%s'\n" "$key" "$escaped_value" >> "$ENV_FILE"
            ;;
    esac
done
printf "HOME='/home/claude'\n" >> "$ENV_FILE"
chown claude:claude "$ENV_FILE"
chmod 600 "$ENV_FILE"

# Restore crontab from persisted task JSON files (survives container recreation)
if ls "$SCHEDULER_DIR/tasks/"*.json >/dev/null 2>&1; then
    CRON_TMP=$(mktemp)
    echo "# Triple-C scheduled tasks — managed by triple-c-scheduler" > "$CRON_TMP"
    echo "# Do not edit manually; changes will be overwritten." >> "$CRON_TMP"
    echo "" >> "$CRON_TMP"
    for task_file in "$SCHEDULER_DIR/tasks/"*.json; do
        [ -f "$task_file" ] || continue
        enabled=$(jq -r '.enabled' "$task_file")
        [ "$enabled" = "true" ] || continue
        schedule=$(jq -r '.schedule' "$task_file")
        id=$(jq -r '.id' "$task_file")
        echo "$schedule /usr/local/bin/triple-c-task-runner $id" >> "$CRON_TMP"
    done
    crontab -u claude "$CRON_TMP" 2>/dev/null || true
    rm -f "$CRON_TMP"
    echo "entrypoint: restored crontab from persisted tasks"
fi

# ── Claude Code self-update ──────────────────────────────────────────────────
# Update the Claude Code CLI to the latest version on container start, before
# any terminal session launches `claude`. Runs as the claude user (the CLI is
# installed under /home/claude/.claude/bin). Non-fatal and time-bounded so a
# slow or offline network never blocks container readiness.
echo "entrypoint: checking for Claude Code updates..."
timeout 120 su -s /bin/bash claude -c 'export PATH="/home/claude/.claude/bin:/home/claude/.local/bin:$PATH"; claude update' \
    && echo "entrypoint: Claude Code is up to date" \
    || echo "entrypoint: warning — Claude Code update skipped or failed (continuing)"

# ── Stay alive as claude ─────────────────────────────────────────────────────
echo "Triple-C container ready."
exec su -s /bin/bash claude -c "exec sleep infinity"
