#!/bin/bash
# NOTE: set -e is intentionally omitted. A failing usermod/groupmod must not
# kill the entire entrypoint — SSH setup, git config, and the final exec
# must still run so the container is usable even if remapping fails.

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

# ── Git credential helper (for HTTPS token) ─────────────────────────────────
if [ -n "$GIT_TOKEN" ]; then
    CRED_FILE="/home/claude/.git-credentials"
    : > "$CRED_FILE"
    chmod 600 "$CRED_FILE"
    chown claude:claude "$CRED_FILE"
    echo "https://oauth2:${GIT_TOKEN}@github.com" >> "$CRED_FILE"
    echo "https://oauth2:${GIT_TOKEN}@gitlab.com" >> "$CRED_FILE"
    echo "https://oauth2:${GIT_TOKEN}@bitbucket.org" >> "$CRED_FILE"
    git config --global --file /home/claude/.gitconfig credential.helper "store --file=$CRED_FILE"
    unset GIT_TOKEN
fi

# ── Git user config ──────────────────────────────────────────────────────────
if [ -n "$GIT_USER_NAME" ]; then
    git config --global --file /home/claude/.gitconfig user.name "$GIT_USER_NAME"
fi
if [ -n "$GIT_USER_EMAIL" ]; then
    git config --global --file /home/claude/.gitconfig user.email "$GIT_USER_EMAIL"
fi
chown claude:claude /home/claude/.gitconfig 2>/dev/null || true

# ── Claude instructions ──────────────────────────────────────────────────────
if [ -n "$CLAUDE_INSTRUCTIONS" ]; then
    mkdir -p /home/claude/.claude
    printf '%s\n' "$CLAUDE_INSTRUCTIONS" > /home/claude/.claude/CLAUDE.md
    chown claude:claude /home/claude/.claude/CLAUDE.md
    unset CLAUDE_INSTRUCTIONS
fi

# ── MCP server configuration ────────────────────────────────────────────────
# Merge MCP server config into ~/.claude.json (preserves existing keys like
# OAuth tokens). Creates the file if it doesn't exist.
if [ -n "$MCP_SERVERS_JSON" ]; then
    CLAUDE_JSON="/home/claude/.claude.json"
    if [ -f "$CLAUDE_JSON" ]; then
        # Merge: existing config + MCP config (MCP keys override on conflict)
        MERGED=$(jq -s '.[0] * .[1]' "$CLAUDE_JSON" <(printf '%s' "$MCP_SERVERS_JSON") 2>/dev/null)
        if [ -n "$MERGED" ]; then
            printf '%s\n' "$MERGED" > "$CLAUDE_JSON"
        else
            echo "entrypoint: warning — failed to merge MCP config into $CLAUDE_JSON"
        fi
    else
        printf '%s\n' "$MCP_SERVERS_JSON" > "$CLAUDE_JSON"
    fi
    chown claude:claude "$CLAUDE_JSON"
    chmod 600 "$CLAUDE_JSON"
    unset MCP_SERVERS_JSON
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

# ── Scheduler setup ─────────────────────────────────────────────────────────
SCHEDULER_DIR="/home/claude/.claude/scheduler"
mkdir -p "$SCHEDULER_DIR/tasks" "$SCHEDULER_DIR/logs" "$SCHEDULER_DIR/notifications"
chown -R claude:claude "$SCHEDULER_DIR"

# Start cron daemon (runs as root, executes jobs per user crontab)
cron

# Save environment variables for cron jobs (cron runs with a minimal env)
ENV_FILE="$SCHEDULER_DIR/.env"
: > "$ENV_FILE"
env | while IFS='=' read -r key value; do
    case "$key" in
        ANTHROPIC_*|AWS_*|CLAUDE_CODE_*|PATH|HOME|LANG|TZ|COLORTERM)
            # Escape single quotes in value and write as KEY='VALUE'
            escaped_value=$(printf '%s' "$value" | sed "s/'/'\\\\''/g")
            printf "%s='%s'\n" "$key" "$escaped_value" >> "$ENV_FILE"
            ;;
    esac
done
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

# ── Stay alive as claude ─────────────────────────────────────────────────────
echo "Triple-C container ready."
exec su -s /bin/bash claude -c "exec sleep infinity"
