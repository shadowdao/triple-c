#!/bin/bash
set -e

# ── UID/GID remapping ──────────────────────────────────────────────────────
# Match the container's claude user to the host user's UID/GID so that
# bind-mounted files (project dir, docker socket) have correct ownership.
if [ -n "$HOST_UID" ] && [ "$HOST_UID" != "$(id -u claude)" ]; then
    usermod -u "$HOST_UID" claude
fi
if [ -n "$HOST_GID" ] && [ "$HOST_GID" != "$(id -g claude)" ]; then
    groupmod -g "$HOST_GID" claude
fi

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
    su -s /bin/bash claude -c "git config --global credential.helper 'store --file=$CRED_FILE'"
    unset GIT_TOKEN
fi

# ── Git user config ──────────────────────────────────────────────────────────
if [ -n "$GIT_USER_NAME" ]; then
    su -s /bin/bash claude -c "git config --global user.name '$GIT_USER_NAME'"
fi
if [ -n "$GIT_USER_EMAIL" ]; then
    su -s /bin/bash claude -c "git config --global user.email '$GIT_USER_EMAIL'"
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

# ── Stay alive as claude ─────────────────────────────────────────────────────
echo "Triple-C container ready."
exec su -s /bin/bash claude -c "exec sleep infinity"
