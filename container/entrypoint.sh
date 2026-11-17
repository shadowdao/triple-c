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

# ── Shared functions ────────────────────────────────────────────────────────
# The CA install, the SSH merge, the git credential helper, the Hybrid home
# links and the repository clone live in entrypoint-lib.sh, which
# triple-c-apply-connect sources too (an AWS workspace whose credentials are
# copied in when the app connects). Sourcing it only defines functions and
# variables; everything still runs from here, in the same order as before.
# The path is fixed, never taken from the environment: this runs as root.
. /usr/local/lib/triple-c/entrypoint-lib.sh \
    || echo "entrypoint: warning — /usr/local/lib/triple-c/entrypoint-lib.sh is missing; the container will be incomplete"

# ── AWS workspace (TRIPLEC_HOST=aws): project data off the environment ─────
# On AWS (an ECS Fargate task) there are no bind mounts and no Docker API.
# Everything project-shaped arrives as env vars injected from the project's
# Secrets Manager secret (app/src-tauri/src/aws/workspace.rs):
#   TRIPLEC_FILES   {"ssh/<file>": base64, "ca/<file>": base64} — decoded here
#                   into /tmp/.host-ssh and /tmp/.host-ca, the paths the
#                   Docker bind mounts use, so the SSH merge and the CA install
#                   below run unchanged;
#   TRIPLEC_REPOS   [{"url","branch"|null,"folder"}] — cloned by clone_repos
#                   once git credentials and ~/.ssh are in place;
#   TRIPLEC_SETUP_SCRIPT  the project's setup script (≤ 32 KiB), written to
#                   /run/triple-c-setup/script by aws_prepare_run and run as
#                   `claude` after the clone (aws_run_setup);
#   TRIPLEC_CREDENTIALS  `connect` when the project copies its credentials in
#                   at connect instead (phase 6 Q2): the secret then has no
#                   TRIPLEC_FILES, API keys, GIT_TOKEN or custom variables, and
#                   the background clone waits (start_clone_repos) for
#                   triple-c-apply-connect to bring them. Not secret; left set.
# All three are unset here, *before* the agent-token block below snapshots the
# environment for the agent: that snapshot is every terminal's environment on
# AWS, and the files hold private keys. The repo list and the setup script are
# kept only as unexported shell variables, which no child process inherits.
#
# Nothing in this block (or any other aws block) runs without TRIPLEC_HOST=aws;
# a Docker container sees the same script it always did.
# >>> aws_env
triplec_aws=0
triplec_connect=0
triplec_repos_json=
triplec_setup_script=

# stdin: the TRIPLEC_FILES JSON; $1 the ssh dir, $2 the CA dir. Names are
# validated (non-empty, no `/`, not `.`/`..`, no NUL) and anything else is
# refused by name. Files are mode 600 in 700 dirs, root-owned when run as root,
# as the read-only bind mounts would present them.
aws_decode_files() {
    python3 -I -c '
import base64, binascii, json, os, stat, sys
dirs = {"ssh": sys.argv[1], "ca": sys.argv[2]}
root = os.geteuid() == 0
def say(msg):
    print("entrypoint: " + msg, file=sys.stderr)
try:
    files = json.load(sys.stdin)
except ValueError:
    say("TRIPLEC_FILES is not valid JSON; no files written")
    sys.exit(1)
if not isinstance(files, dict):
    say("TRIPLEC_FILES is not a JSON object; no files written")
    sys.exit(1)
def write(key, value):
    kind, sep, name = key.partition("/")
    if kind not in dirs or not sep or name in ("", ".", "..") or "/" in name or "\0" in name:
        return "not ssh/<name> or ca/<name>"
    name.encode("utf-8")  # a lone surrogate cannot name a file
    try:
        data = base64.b64decode(value, validate=True) if isinstance(value, str) else None
    except (binascii.Error, ValueError):
        data = None
    if data is None:
        return "value is not base64"
    d = dirs[kind]
    os.makedirs(d, mode=0o700, exist_ok=True)
    if not stat.S_ISDIR(os.lstat(d).st_mode):
        return "%s is not a directory" % d
    os.chmod(d, 0o700)
    if root:
        os.chown(d, 0, 0)
    fd = os.open(os.path.join(d, name), os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as f:
        os.fchmod(f.fileno(), 0o600)
        if root:
            os.fchown(f.fileno(), 0, 0)
        f.write(data)
    return None
bad = 0
for key, value in files.items():
    try:
        why = write(key, value)
    except (OSError, UnicodeError, ValueError) as e:
        why = "%s" % (e.strerror if isinstance(e, OSError) and e.strerror else type(e).__name__)
    if why:
        say("refusing file entry %r: %s" % (key, why))
        bad += 1
sys.exit(1 if bad else 0)
' "$1" "$2"
}

if [ "${TRIPLEC_HOST:-}" = aws ]; then
    triplec_aws=1
    if [ -n "${TRIPLEC_FILES+x}" ]; then
        printf '%s' "$TRIPLEC_FILES" | aws_decode_files /tmp/.host-ssh /tmp/.host-ca \
            || echo "entrypoint: warning — some TRIPLEC_FILES entries were not written (see above)"
        unset TRIPLEC_FILES
    fi
    triplec_repos_json="${TRIPLEC_REPOS:-}"
    unset TRIPLEC_REPOS
    triplec_setup_script="${TRIPLEC_SETUP_SCRIPT:-}"
    unset TRIPLEC_SETUP_SCRIPT
    # Copy-at-connect: no credentials in the secret; the clone waits for the
    # app's first connect to bring them (triple-c-apply-connect).
    [ "${TRIPLEC_CREDENTIALS:-}" = connect ] && triplec_connect=1
fi
# <<< aws_env

# ── AWS: this run's record, and the setup script's files ───────────────────
# aws_prepare_run, as root at the start of an AWS task (just after the
# agent-token block):
#   /run/triple-c            root, 0711 — users may open a file they know the
#                            name of, never list it (the stop marker in it is
#                            0600; the stop guard and the idle watcher keep the
#                            mode);
#   /run/triple-c/run-id     `<UTC yyyymmdd-HHMMSS>-<task id, first 8>` (task id
#                            from the ECS task metadata, else `local`), 0644 —
#                            triple-c-wip-push names this run's WIP branches
#                            after it (triple-c/wip/<run-id>-<folder>);
#   /run/triple-c/start-epoch  the start in Unix seconds, for the idle watcher's
#                            lifetime cap (TRIPLEC_MAX_LIFETIME_MINUTES);
#   /run/triple-c/wip-last   0700, owned by `claude`: triple-c-wip-push (run as
#                            claude) records there the tree it last pushed per
#                            folder and skips an unchanged one. Root makes it
#                            and never looks inside;
#   /run/triple-c/repos.json the repository list (TRIPLEC_REPOS), root 0600:
#                            triple-c-apply-connect re-clones the ones that
#                            failed from it. `connected` (its marker) is
#                            cleared;
#   /run/triple-c-setup      root, 0755: `script` (0644) when the project has a
#                            setup script, `log` (0644; root opens it and hands
#                            the script the descriptor, so the user never needs
#                            write access to anything here and root never opens
#                            a path the user controls) and `status` — `running`
#                            from now until the script ends (waiting for the
#                            clone included), then `ok` or `failed <code>`. No
#                            status file: no setup script.
# aws_run_setup runs the script as `claude`, `bash -e`, stdin /dev/null, cwd
# /workspace, under `timeout -k 10 1800`, chained after the background clone
# (start_clone_repos) so the agent's start is never held up. A failure is
# recorded, never fatal. triple-c-workspace-status reads all of this back.
# >>> aws_run
triplec_setup_dir=/run/triple-c-setup
triplec_setup_timeout=1800

# The first 8 characters of this task's id, from the ECS task metadata
# endpoint ECS sets for every container; `local` without one or on any doubt.
aws_task_id8() {
    local uri="${ECS_CONTAINER_METADATA_URI_V4:-}" arn id
    if [ -n "$uri" ]; then
        arn=$(curl -sf --max-time 3 "$uri/task" 2>/dev/null | jq -r '.TaskARN // empty' 2>/dev/null)
        id=${arn##*/}
        case "$id" in
            ''|*[!A-Za-z0-9]*) ;;
            *) [ "${#id}" -ge 8 ] && { printf '%s\n' "${id:0:8}"; return; } ;;
        esac
    fi
    echo local
}

aws_prepare_run() {
    local now root=0
    [ "$(id -u)" = 0 ] && root=1
    now=$(date +%s)
    if mkdir -p "$triplec_run_dir"; then
        [ "$root" = 1 ] && chown root:root "$triplec_run_dir"
        chmod 711 "$triplec_run_dir"
        printf '%s-%s\n' "$(date -u -d "@$now" +%Y%m%d-%H%M%S)" "$(aws_task_id8)" > "$triplec_run_dir/run-id"
        chmod 644 "$triplec_run_dir/run-id"
        printf '%s\n' "$now" > "$triplec_run_dir/start-epoch"
        rm -rf "$triplec_run_dir/wip-last"
        if mkdir -m 700 "$triplec_run_dir/wip-last" && [ -n "${triplec_run_as:-}" ]; then
            chown "$triplec_run_as:" "$triplec_run_dir/wip-last"
        fi
        rm -f "$triplec_run_dir/connected"
        ( umask 077; printf '%s' "$triplec_repos_json" > "$triplec_run_dir/repos.json" )
    else
        echo "entrypoint: warning — could not create $triplec_run_dir"
    fi
    rm -rf "$triplec_setup_dir"
    mkdir -m 755 "$triplec_setup_dir" || { echo "entrypoint: warning — could not create $triplec_setup_dir; no setup"; return 0; }
    [ "$root" = 1 ] && chown root:root "$triplec_setup_dir"
    [ -n "$triplec_setup_script" ] || return 0
    printf '%s' "$triplec_setup_script" > "$triplec_setup_dir/script"
    chmod 644 "$triplec_setup_dir/script"
    : > "$triplec_setup_dir/log"
    chmod 644 "$triplec_setup_dir/log"
    printf 'running\n' > "$triplec_setup_dir/status"
    triplec_setup_script=
}

# The setup chain ended without a verdict: record a failure (no code).
aws_setup_abandoned() {
    [ "$(head -n 1 "$triplec_setup_dir/status" 2>/dev/null)" = running ] || return 0
    printf 'failed\n' > "$triplec_setup_dir/status.tmp" && mv -f "$triplec_setup_dir/status.tmp" "$triplec_setup_dir/status"
    echo "entrypoint: the setup script did not finish — recorded as failed"
}

# Runs the setup script, if there is one (see above). Never fails.
aws_run_setup() {
    local dir="$triplec_setup_dir" rc state
    [ -f "$dir/script" ] || return 0
    echo "entrypoint: running the setup script..."
    (
        cd /workspace 2>/dev/null || cd /
        _triplec_as_user timeout -k 10 "$triplec_setup_timeout" bash -e "$dir/script"
    ) </dev/null >>"$dir/log" 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then state=ok; else state="failed $rc"; fi
    printf '%s\n' "$state" > "$dir/status.tmp" && mv -f "$dir/status.tmp" "$dir/status"
    echo "entrypoint: setup script $state"
    return 0
}
# <<< aws_run

# ── triple-c-agent token: off the environment before anything else runs ───
# The agent (started at the very end, see start_agent) authenticates every
# request with this token, and it runs as root, so the token is effectively
# root in this container. Given as an env var it would be inherited by every
# process this script starts (cron, the scheduler, Claude sessions spawned by
# the agent). So it moves to a root-only file *first* and leaves the
# environment; the agent reads the file. A `docker exec` session still sees
# the container's configured env — the local test harness passes it that way
# and accepts that; on AWS nothing is started by `docker exec`.
#
# The agent also takes its exec environment and default working directory
# from how *it* was started, and execs must see what `docker exec` would: the
# container's configured env and WorkingDir, not whatever this script has
# exported or cd'd into by the time it gets to start_agent. So both are
# snapshotted here, before anything else runs, and the agent is started from
# the snapshot (`env -i`). The agent drops its own TRIPLEC_AGENT_* settings.
# >>> agent_token
agent_cwd=$PWD
# The token goes on a tmpfs, never the container's writable layer: a file
# there would persist after stop and ride `docker commit` into the project's
# snapshot image (the commit's SECRET_ENV_KEYS scrub covers env, not files).
# /dev/shm is a tmpfs in Docker and on Fargate; anything else is refused.
if [ -n "${TRIPLEC_AGENT_TOKEN:-}" ]; then
    if [ "$(stat -f -c %T /dev/shm 2>/dev/null)" = tmpfs ]; then
        mkdir -p /dev/shm/triple-c
        chown root:root /dev/shm/triple-c
        chmod 700 /dev/shm/triple-c
        ( umask 077; printf '%s' "$TRIPLEC_AGENT_TOKEN" > /dev/shm/triple-c/agent-token )
        export TRIPLEC_AGENT_TOKEN_FILE=/dev/shm/triple-c/agent-token
    else
        echo "triple-c-agent: /dev/shm is not a tmpfs; not writing the token to disk — agent disabled" >&2
    fi
    unset TRIPLEC_AGENT_TOKEN
fi
# The exec environment snapshot stays in this script's memory — it holds the
# container's whole configured env, API keys included, so it must not touch
# disk either. Minus what bash itself exports (PWD, OLDPWD, SHLVL, `_`):
# those are this script's, not the container's, and `docker exec` has none.
agent_env=()
if [ -n "${TRIPLEC_AGENT_TOKEN_FILE:-}" ]; then
    mapfile -d '' agent_env < <(env -u PWD -u OLDPWD -u SHLVL -u _ -0)
fi
# <<< agent_token
# After the token block, so nothing aws_prepare_run starts (the metadata curl)
# inherits TRIPLEC_AGENT_TOKEN; before anything that can push.
if [ "$triplec_aws" = 1 ]; then aws_prepare_run; fi

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

# On AWS nothing is bind-mounted and HOST_UID/HOST_GID are never set, so there
# is nothing to match; skipped outright rather than relying on that.
[ "$triplec_aws" = 1 ] || remap_uid_gid

# Fix ownership of home directory after UID/GID change — only where it is
# wrong. A blanket `chown -R` rewrites every inode, and where /home/claude is
# the image's own copy rather than a volume (an ECS/Fargate task) that copies
# the whole directory up into the writable layer: measured at ~5 minutes on
# 1 vCPU before the agent could start. `find` only stats. `-h` and find's
# default no-follow match `chown -R`'s physical walk (it changes a symlink
# itself, never its target), and like `chown -R` this crosses into the nested
# ~/.claude volume.
find /home/claude \( ! -user claude -o ! -group claude \) -exec chown -h claude:claude {} +

if [ "$triplec_aws" = 1 ] && aws_storage_links_home "${TRIPLEC_STORAGE:-}"; then
    link_persistent_home /home/claude "$triplec_home_mnt" claude:claude
fi
if [ "$triplec_aws" = 1 ] && [ "${TRIPLEC_STORAGE:-}" = efs ]; then
    aws_efs_workspace_check /workspace
fi


install_corporate_ca


merge_host_ssh /tmp/.host-ssh

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
    install_git_credentials "$GIT_TOKEN"
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
# >>> feature_skills
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
# <<< feature_skills

# ── Claude Code settings ────────────────────────────────────────────────────
# Apply the managed Claude Code settings to ~/.claude/settings.json, keeping
# every key the user set inside the container.
#
# `settings.json` lives on the persisted triple-c-claude-config-{id} volume, so
# it outlives the container and a plain `.[0] * .[1]` merge could only ever
# *add*. That is what made every one of these settings one-way: switching one
# off in Triple-C omitted its key, the merge preserved the old on-value, and the
# setting stayed on until a destructive Reset. So Rust states the whole managed
# key set on every start, in two parts: CLAUDE_CODE_SETTINGS_JSON holds the keys
# that have a value, and CLAUDE_CODE_SETTINGS_CLEAR is a JSON array of the key
# names whose neutral state is *unset* (`tui`, `effortLevel`, `viewMode`,
# `awaySummaryEnabled`) and which must therefore be deleted rather than pinned
# to a stand-in value.
#
# The two are kept apart rather than using a null-means-delete payload because
# a project recreates from its own snapshot image, which carries whatever
# entrypoint it was built with. An older one merges with a plain `.[0] * .[1]`
# and would write literal nulls into the user's settings.json; it ignores
# CLAUDE_CODE_SETTINGS_CLEAR instead, keeping the old sticky behaviour until the
# project is migrated or Reset.
# See `build_claude_code_settings_json` in app/src-tauri/src/docker/container.rs.
if [ -n "$CLAUDE_CODE_SETTINGS_JSON" ]; then
    SETTINGS_FILE="/home/claude/.claude/settings.json"
    mkdir -p /home/claude/.claude
    # One code path for "file exists" and "file doesn't".
    [ -f "$SETTINGS_FILE" ] || printf '{}\n' > "$SETTINGS_FILE"
    MERGED=$(jq -s --argjson clear "${CLAUDE_CODE_SETTINGS_CLEAR:-[]}" '
        .[0] as $current
        | .[1] as $set
        | ($current * $set) | delpaths($clear | map([.]))
    ' "$SETTINGS_FILE" <(printf '%s' "$CLAUDE_CODE_SETTINGS_JSON") 2>/dev/null)
    if [ -n "$MERGED" ]; then
        printf '%s\n' "$MERGED" > "$SETTINGS_FILE"
    else
        echo "entrypoint: warning — failed to merge Claude Code settings into $SETTINGS_FILE"
    fi
    chown claude:claude "$SETTINGS_FILE"
    chmod 600 "$SETTINGS_FILE"
    unset CLAUDE_CODE_SETTINGS_JSON
    unset CLAUDE_CODE_SETTINGS_CLEAR
fi

# ── AWS SSO auth refresh command ──────────────────────────────────────────────
# When set (Bedrock + profile/SSO auth), inject awsAuthRefresh into
# ~/.claude.json so Claude Code calls triple-c-sso-refresh when AWS credentials
# expire mid-session. When NOT set, strip any awsAuthRefresh left behind by a
# previous Bedrock-profile session — ~/.claude.json lives in the persisted home
# volume, so without this the container keeps trying to run the SSO refresh even
# after switching to a non-SSO backend (Anthropic/Ollama) or to static creds.
# Replace ~/.claude.json atomically: write a sibling temp file, then rename.
#
# `> "$CLAUDE_JSON"` truncates before it writes, so a write that fails part-way
# — a full home volume being the obvious way, and bounding that volume is what
# half this release is about — leaves the file unparseable. It holds the OAuth
# account, and the damage does not self-heal: the next start's `jq` fails on the
# corrupt file, `MERGED` comes back empty, and the `[ -n "$MERGED" ]` guard
# skips the write that would have repaired it. `triple-c-task-runner` has done
# it this way all along.
#
# On AWS Hybrid storage ~/.claude.json is a link into the EFS mount, and
# renaming over the link would swap it for a plain file and the state would
# quietly stop persisting. So the write goes to the link's target — but only
# when the link is exactly the one link_persistent_home makes and its target
# is not itself a link, and it is done *as claude*: that directory is
# claude-writable, so root must not write, rename or chown anything in it.
# Docker mode never takes that branch.
# >>> write_claude_json
write_claude_json() {
    if [ "${triplec_aws:-0}" = 1 ] && [ -L "$CLAUDE_JSON" ]; then
        _wcj_dest="$triplec_home_mnt/.claude.json"
        if [ "$(readlink "$CLAUDE_JSON")" != "$_wcj_dest" ] || [ -L "$triplec_home_mnt" ] || [ -L "$_wcj_dest" ]; then
            echo "entrypoint: warning — $CLAUDE_JSON is a symlink Triple-C did not make; not writing through it"
            return 1
        fi
        if printf '%s\n' "$1" | _triplec_as_user sh -c \
            'umask 077; cat > "$1.triple-c-tmp" && mv -f "$1.triple-c-tmp" "$1"' _ "$_wcj_dest"; then
            return 0
        fi
        _triplec_as_user rm -f "$_wcj_dest.triple-c-tmp"
        echo "entrypoint: warning — could not write $CLAUDE_JSON (leaving it as it was)"
        return 1
    fi
    _wcj_tmp="${CLAUDE_JSON}.triple-c-tmp"
    if printf '%s\n' "$1" > "$_wcj_tmp" 2>/dev/null; then
        mv -f "$_wcj_tmp" "$CLAUDE_JSON" 2>/dev/null || rm -f "$_wcj_tmp"
    else
        rm -f "$_wcj_tmp"
        echo "entrypoint: warning — could not write $CLAUDE_JSON (leaving it as it was)"
        return 1
    fi
    # By name, after the rename, so these land on the new inode.
    chown claude:claude "$CLAUDE_JSON"
    chmod 600 "$CLAUDE_JSON"
}
# <<< write_claude_json

CLAUDE_JSON="/home/claude/.claude.json"
if [ -n "$AWS_SSO_AUTH_REFRESH_CMD" ]; then
    if [ -f "$CLAUDE_JSON" ]; then
        MERGED=$(jq --arg cmd "$AWS_SSO_AUTH_REFRESH_CMD" '.awsAuthRefresh = $cmd' "$CLAUDE_JSON" 2>/dev/null)
        if [ -n "$MERGED" ]; then
            write_claude_json "$MERGED"
        fi
    else
        # No existing file, so there is nothing to destroy — but go through the
        # same helper so the owner and mode are set in one place.
        write_claude_json "$(printf '{"awsAuthRefresh":"%s"}' "$AWS_SSO_AUTH_REFRESH_CMD")"
    fi
    unset AWS_SSO_AUTH_REFRESH_CMD
elif [ -f "$CLAUDE_JSON" ] && grep -q '"awsAuthRefresh"' "$CLAUDE_JSON" 2>/dev/null; then
    # Only rewrite when the key is actually present, to avoid a needless jq
    # reformat of ~/.claude.json on every start of a non-SSO backend.
    MERGED=$(jq 'del(.awsAuthRefresh)' "$CLAUDE_JSON" 2>/dev/null)
    if [ -n "$MERGED" ]; then
        write_claude_json "$MERGED"
    fi
fi

# ── Shift+Enter key binding ───────────────────────────────────────────────────
# Triple-C's terminals send ESC+CR (`\e\r`) on Shift+Enter — the same bytes
# `/terminal-setup` installs for VS Code, Cursor, Alacritty and Zed — and Claude
# Code decodes those unconditionally. What it cannot see is that the binding is
# already in place, so it keeps printing its "run /terminal-setup" tip. This
# flag is what that tip is gated on: purely cosmetic, and it changes nothing
# about how the sequence is decoded.
#
# $CLAUDE_JSON is already set by the awsAuthRefresh block above.
if [ -f "$CLAUDE_JSON" ]; then
    # Only rewrite when the value isn't already true, to avoid a needless jq
    # reformat of ~/.claude.json on every single start.
    if ! grep -q '"shiftEnterKeyBindingInstalled"[[:space:]]*:[[:space:]]*true' "$CLAUDE_JSON" 2>/dev/null; then
        # Atomic, via `write_claude_json` — see its comment for why a plain
        # `>` on this file can permanently destroy the OAuth login.
        MERGED=$(jq '.shiftEnterKeyBindingInstalled = true' "$CLAUDE_JSON" 2>/dev/null)
        if [ -n "$MERGED" ]; then
            write_claude_json "$MERGED"
        fi
    fi
else
    # Nothing to destroy, but the helper owns the owner/mode too.
    write_claude_json '{"shiftEnterKeyBindingInstalled":true}'
fi

# ── Docker socket permissions ────────────────────────────────────────────────
# Docker mode and AWS alike: an EC2 project with Docker access mounts its own
# instance's socket here (phase 7 E6); Fargate never has one.
# >>> docker_socket
if [ -S /var/run/docker.sock ]; then
    DOCKER_GID=$(stat -c '%g' /var/run/docker.sock)
    if ! getent group "$DOCKER_GID" > /dev/null 2>&1; then
        groupadd -g "$DOCKER_GID" docker-host
    fi
    DOCKER_GROUP=$(getent group "$DOCKER_GID" | cut -d: -f1)
    usermod -aG "$DOCKER_GROUP" claude
fi
# <<< docker_socket

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
# The lock is shared with the per-session update that every Claude terminal
# runs before `exec claude` (commands/terminal_commands.rs, UPDATE_PRELUDE).
# "Container ready" is printed *after* this finishes, so a user who starts a
# project and immediately opens a tab would otherwise have two updaters
# rewriting ~/.claude/bin at once, and the session's `|| echo` would hide the
# damage right before it ran the result.
echo "entrypoint: checking for Claude Code updates..."
timeout 120 su -s /bin/bash claude -c 'export PATH="/home/claude/.claude/bin:/home/claude/.local/bin:$PATH"; flock -w 90 -E 0 /tmp/.triple-c-claude-update.lock claude update' \
    && echo "entrypoint: Claude Code is up to date" \
    || echo "entrypoint: warning — Claude Code update skipped or failed (continuing)"

# ── Stay alive, as root ──────────────────────────────────────────────────────
# The container lives exactly as long as this script: docker-init is PID 1 and
# exits when its one child does. So what keeps it alive must be something the
# claude user cannot kill. This used to end in
#   exec su -s /bin/bash claude -c "exec sleep infinity"
# which hung the container's whole lifetime on a `sleep` owned by claude — and
# Claude Code runs as claude and routinely runs `pkill sleep`, `pkill -f …`,
# `killall` or `kill -9 -1` while testing something. Any of those stopped the
# container: every terminal disconnected and the app still said Running.
# Dropping to claude there bought nothing (it dates from the initial commit,
# with no stated reason, and nothing reads that process's owner); its one
# reader is the app's marketplace readiness probe, which now also matches the
# marker below (app/src-tauri/src/marketplace/sync.rs, READY_PROBE).
#
#  * Unprivileged kills get EPERM on both this script and its sleep. Even
#    `sudo pkill sleep` only kills the child, which the loop replaces; it takes
#    a root kill of this script itself.
#  * `docker stop`: docker-init forwards SIGTERM here, and bash runs the trap
#    as soon as `wait` returns, so the container stops at once rather than
#    sitting out Docker's grace period and being SIGKILLed.
#  * The sleep runs as `triple-c-keepalive infinity` (exec -a), a root-owned
#    command line nothing else has, and it exists only once every step above
#    has finished — that is the readiness marker. Keep the name in step with
#    READY_PROBE.
#  * Respawns are throttled to about one a second. If the sleep cannot start
#    (fork failing under a pids limit or a fork bomb) or dies at once, an
#    unthrottled loop would spin a core at 100% exactly when the container is
#    already in trouble. The pause is `read -t` on a FIFO nobody writes —
#    a builtin, so it still works when fork does not — with `sleep 1` as the
#    fallback should the FIFO be unavailable.
# ── triple-c-agent ──────────────────────────────────────────────────────────
# The app's way into the container on hosts without a Docker API (AWS, over
# an SSM port-forward). Started only when a token was provided, and only now,
# after every setup step above, so "the agent answers /v1/health" also means
# "the container is set up" — the AWS host's readiness signal.
#
# Same survival rules as the keepalive below, for the same reason: it runs as
# root under a root supervisor loop, so a claude-user `pkill`/`kill -9 -1`
# gets EPERM, and a crash is restarted (throttled: a binary that exits at
# once must not spin a core).
# >>> start_agent
start_agent() {
    [ -n "${TRIPLEC_AGENT_TOKEN_FILE:-}" ] || return 0
    if [ ! -x /usr/local/bin/triple-c-agent ]; then
        echo "triple-c-agent: token given but no agent binary in this image — not starting it" >&2
        return 0
    fi
    (
        # Keeps the supervisor alive through a stray HUP. The agent resets
        # every signal disposition and the mask in each exec it spawns, so this
        # does not leak into commands (an ignored SIGHUP otherwise survives
        # exec and turns every exec into `nohup`).
        trap '' HUP
        cd "$agent_cwd" || cd /
        while :; do
            agent_started=$SECONDS
            env -i "${agent_env[@]}" /usr/local/bin/triple-c-agent
            echo "triple-c-agent exited ($?) — restarting" >&2
            [ $((SECONDS - agent_started)) -ge 5 ] || sleep 5
        done
    ) &
}
# <<< start_agent
start_agent

# ── AWS: WIP push on stop, idle auto-stop, lifetime cap ─────────────────────
# aws_before_stop runs from the TERM trap below, before the keepalive goes:
# with TRIPLEC_WIP_PUSH=1 it pushes every repo's unsaved work to the run's
# triple-c/wip/<run-id> branch (as `claude`, bounded at 90 s), unless the app's
# stop guard recorded `pushed` or `discard` in /run/triple-c/stop-reason
# (written as root through the agent just before StopTask), or the idle
# watcher recorded `idle` (it pushes *before* signalling and only signals once
# nothing unsaved is left, so an idle stop never loses work) or `lifetime`
# (it tried its push, best effort, before signalling). ECS gives a task
# `stopTimeout` seconds between SIGTERM and SIGKILL — 30 by default, 120 at
# most on Fargate — so the task definition must raise it for the push to
# finish. Once only: a second TERM during the push changes nothing.
#
# The idle watcher (TRIPLEC_IDLE_STOP_MINUTES and/or a non-zero
# TRIPLEC_MAX_LIFETIME_MINUTES set) runs as root under a
# supervisor loop like the agent's, so a claude-user `pkill` cannot switch
# auto-stop off. When the workspace is idle it records the reason "idle" and
# sends TERM to *this* script ($$) — not PID 1, which under ECS's
# initProcessEnabled is docker-init — so an idle stop and an ECS stop take
# this same trap and push at most once. An idle stop exits with status 75
# (triplec_idle_exit; Rust `aws::tasks::IDLE_EXIT_CODE`), so ECS reports it as
# EssentialContainerExited with that exit code and the app can say "stopped
# after being idle" rather than "crashed". A lifetime-cap stop (the run's
# start-epoch plus TRIPLEC_MAX_LIFETIME_MINUTES reached; advisory — a user
# with sudo can switch it off) exits 76 (triplec_lifetime_exit; Rust
# `aws::tasks::LIFETIME_EXIT_CODE`). Its environment is built from scratch: it
# needs none of the container's but TRIPLEC_WIP_PUSH and the lifetime cap.
# >>> aws_stop
triplec_idle_exit=75
triplec_lifetime_exit=76
# Persistent terminals (one tmux server per app tab, `tmux -L triple-c-<id>`,
# app/src-tauri/src/host/persistent.rs) outlive the app, so a detached shell or
# Claude could still be writing while the push below takes its snapshot: end
# them first (as the user that owns them; bounded wait for the servers to go).
# The app's own Stop does the same before its push.
aws_end_terminals() {
    command -v tmux >/dev/null 2>&1 || return 0
    _triplec_as_user sh -c '
        d="${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)"
        for s in "$d"/triple-c-*; do
            [ -S "$s" ] && tmux -L "${s##*/}" kill-server 2>/dev/null
        done
        i=0
        while pgrep -u "$(id -u)" -f "^tmux -L triple-c-" >/dev/null 2>&1 && [ $i -lt 50 ]; do
            sleep 0.1; i=$((i + 1))
        done
        exit 0'
}
aws_before_stop() {
    [ "${TRIPLEC_HOST:-}" = aws ] || return 0
    [ -z "${triplec_stopping:-}" ] || return 0
    triplec_stopping=1
    local reason=sigterm recorded
    recorded=$(cat /run/triple-c/stop-reason 2>/dev/null)
    case "$recorded" in
        idle)
            # The watcher already pushed (or found nothing to push).
            triplec_exit_status=$triplec_idle_exit
            echo "entrypoint: stopping (idle) — work in progress already handled by the idle watcher"
            return 0 ;;
        lifetime)
            # The watcher tried its push (best effort) before signalling.
            triplec_exit_status=$triplec_lifetime_exit
            echo "entrypoint: stopping (lifetime) — the environment's maximum workspace lifetime was reached; work in progress already handled by the idle watcher"
            return 0 ;;
        # The app's stop guard already pushed, or the user chose to stop
        # without pushing: honour that instead of pushing again.
        pushed|discard)
            echo "entrypoint: stopping — work in progress already handled by Triple-C ($recorded); not pushing"
            return 0 ;;
    esac
    if [ "${TRIPLEC_WIP_PUSH:-0}" = 1 ]; then
        aws_end_terminals
        echo "entrypoint: stopping ($reason) — pushing work in progress..."
        _triplec_as_user timeout -k 5 90 /usr/local/bin/triple-c-wip-push --reason "$reason" \
            || echo "entrypoint: warning — WIP push incomplete (see above)"
    fi
    return 0
}

start_idle_watch() {
    local minutes="$1" signal_pid="$2"
    mkdir -p /run/triple-c && chmod 711 /run/triple-c
    rm -f /run/triple-c/stop-reason
    if [ -n "$minutes" ] && [ -z "${TRIPLEC_AGENT_TOKEN_FILE:-}" ]; then
        echo "triple-c-idle-watch: no agent token, so no activity to read — idle stop will never fire" >&2
    fi
    (
        trap '' HUP
        while :; do
            watch_started=$SECONDS
            env -i PATH="$PATH" \
                TRIPLEC_IDLE_STOP_MINUTES="$minutes" \
                TRIPLEC_IDLE_SIGNAL_PID="$signal_pid" \
                TRIPLEC_AGENT_TOKEN_FILE="${TRIPLEC_AGENT_TOKEN_FILE:-/dev/shm/triple-c/agent-token}" \
                TRIPLEC_WIP_PUSH="${TRIPLEC_WIP_PUSH:-0}" \
                TRIPLEC_MAX_LIFETIME_MINUTES="${TRIPLEC_MAX_LIFETIME_MINUTES:-}" \
                /usr/local/bin/triple-c-idle-watch
            rc=$?
            # 0: it signalled the stop. 2: misconfigured, and a restart would
            # only say so again.
            [ "$rc" -eq 0 ] || [ "$rc" -eq 2 ] && break
            echo "triple-c-idle-watch exited ($rc) — restarting" >&2
            [ $((SECONDS - watch_started)) -ge 5 ] || sleep 5
        done
    ) &
}
# <<< aws_stop
# The stop trap goes in before any background work starts (the idle watcher
# can signal this script; a clone is work a stop must account for), so a TERM
# from here on always runs aws_before_stop. Defined after aws_stop, which
# holds the function it calls.
keepalive_pid=""
stop_keepalive() {
    [ "$triplec_aws" = 1 ] && aws_before_stop
    [ -n "$keepalive_pid" ] && kill "$keepalive_pid" 2>/dev/null
    exit "${triplec_exit_status:-0}"
}
trap stop_keepalive TERM INT HUP
if [ "$triplec_aws" = 1 ] && { [ -n "${TRIPLEC_IDLE_STOP_MINUTES:-}" ] || [ -n "${TRIPLEC_MAX_LIFETIME_MINUTES//0/}" ]; }; then
    start_idle_watch "${TRIPLEC_IDLE_STOP_MINUTES:-}" "$$"
fi
# Repos are cloned in the background from here on — see clone_repos — and the
# setup script runs after them (aws_run_setup).
if [ "$triplec_aws" = 1 ]; then
    start_clone_repos "$triplec_repos_json" /run/triple-c-clone
fi

echo "Triple-C container ready."
throttle_fifo=/run/triple-c-keepalive.throttle
rm -f "$throttle_fifo"
mkfifo -m 600 "$throttle_fifo" 2>/dev/null && exec 3<>"$throttle_fifo"
rm -f "$throttle_fifo"
respawn_pause() {
    read -r -t 1 -u 3 _ 2>/dev/null
    # >128 is read's timeout, i.e. it paused; anything else means no FIFO.
    [ $? -gt 128 ] || sleep 1
}
while :; do
    started=$SECONDS
    (exec -a triple-c-keepalive sleep infinity 3>&-) &
    keepalive_pid=$!
    wait "$keepalive_pid"
    [ $((SECONDS - started)) -ge 2 ] || respawn_pause
done
