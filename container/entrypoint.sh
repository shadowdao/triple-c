#!/bin/bash
set -e

# ── SSH key permissions ──────────────────────────────────────────────────────
# If SSH keys were mounted, fix permissions (bind mounts may have wrong perms)
if [ -d /home/claude/.ssh ]; then
    chmod 700 /home/claude/.ssh
    find /home/claude/.ssh -type f -name "id_*" ! -name "*.pub" -exec chmod 600 {} \;
    find /home/claude/.ssh -type f -name "*.pub" -exec chmod 644 {} \;
    # Write known_hosts fresh (not append) to avoid duplicates across restarts
    ssh-keyscan -t ed25519,rsa github.com gitlab.com bitbucket.org > /home/claude/.ssh/known_hosts 2>/dev/null || true
    chmod 644 /home/claude/.ssh/known_hosts
fi

# ── Git credential helper (for HTTPS token) ─────────────────────────────────
if [ -n "$GIT_TOKEN" ]; then
    # Use git credential-store with a protected file instead of embedding in config
    CRED_FILE="/home/claude/.git-credentials"
    : > "$CRED_FILE"
    chmod 600 "$CRED_FILE"
    echo "https://oauth2:${GIT_TOKEN}@github.com" >> "$CRED_FILE"
    echo "https://oauth2:${GIT_TOKEN}@gitlab.com" >> "$CRED_FILE"
    echo "https://oauth2:${GIT_TOKEN}@bitbucket.org" >> "$CRED_FILE"
    git config --global credential.helper "store --file=$CRED_FILE"
    # Clear the env var so it's not visible in /proc/*/environ
    unset GIT_TOKEN
fi

# ── Git user config ──────────────────────────────────────────────────────────
if [ -n "$GIT_USER_NAME" ]; then
    git config --global user.name "$GIT_USER_NAME"
fi
if [ -n "$GIT_USER_EMAIL" ]; then
    git config --global user.email "$GIT_USER_EMAIL"
fi

# ── Docker socket permissions ────────────────────────────────────────────────
if [ -S /var/run/docker.sock ]; then
    DOCKER_GID=$(stat -c '%g' /var/run/docker.sock)
    if ! getent group "$DOCKER_GID" > /dev/null 2>&1; then
        sudo groupadd -g "$DOCKER_GID" docker-host
    fi
    DOCKER_GROUP=$(getent group "$DOCKER_GID" | cut -d: -f1)
    sudo usermod -aG "$DOCKER_GROUP" claude
fi

# ── Stay alive ───────────────────────────────────────────────────────────────
echo "Triple-C container ready."
exec sleep infinity
