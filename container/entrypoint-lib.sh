#!/bin/bash
# Triple-C entrypoint library — sourced, never run.
#
# Installed at /usr/local/lib/triple-c/entrypoint-lib.sh (root, 0644) and
# sourced by both /usr/local/bin/entrypoint.sh (every container start, Docker
# and AWS) and /usr/local/bin/triple-c-apply-connect (an AWS workspace whose
# credentials are copied in when the app connects, phase 6 Q2), so the CA
# install, the SSH merge, the git credential helper and the clone are one
# piece of code for both. **Sourcing it only defines variables and functions;
# it runs nothing** — app/src-tauri/src/docker/aws_mode_tests.rs checks that.
#
# The `# >>> name` / `# <<< name` markers delimit blocks the Rust tests cut out
# and run under bash (aws_mode_tests.rs, image.rs's sync_host_ssh tests); keep
# each block self-contained.

# ── Shared settings and the drop to the container user ─────────────────────
# >>> aws_lib
triplec_run_as=claude
# Where Hybrid storage mounts the project's EFS access point.
triplec_home_mnt=/mnt/triple-c-home
# This run's record (aws_prepare_run in entrypoint.sh) and, under
# copy-at-connect, the marker triple-c-apply-connect leaves there.
triplec_run_dir=/run/triple-c
# Copy-at-connect: how long the background clone waits for the app's first
# connect to bring the credentials (aws_wait_connect).
triplec_connect_wait=300

# Runs a command as the container user (`claude`) when this script is root.
_triplec_as_user() {
    if [ -n "${triplec_run_as:-}" ] && [ "$(id -u)" = 0 ]; then
        su -s /bin/bash -c '"$@"' -- "$triplec_run_as" _ "$@"
    else
        "$@"
    fi
}
# <<< aws_lib

# ── AWS Hybrid/EFS storage: persisted home state (plan ruling P4) ───────────
# EFS storage is Hybrid plus /workspace on a second access point (mounted by
# the task definition straight at /workspace), so it gets the same home links.
# A Hybrid workspace mounts its EFS access point (POSIX 1000:1000) at
# /mnt/triple-c-home. Mounting it over /home/claude would hide the image's
# Claude Code, cargo and uv — EFS has no Docker-volume "copy the image's
# contents on first mount" — so only user state is persisted, by symlink:
# ~/.claude, ~/.claude.json and ~/.ssh. On first use each is seeded from the
# image's copy (or created empty); from then on the persisted copy wins and
# the fresh image copy every new task starts with is replaced by the link —
# what a Docker home volume does by masking. Runs before the SSH merge below,
# so host keys land on EFS, and before anything writes ~/.claude.
# >>> aws_home
link_persistent_home() {
    local home="$1" persist="$2" owner="$3" name link target names=".claude .claude.json .ssh"
    # Copy-at-connect (ruling R6): ~/.ssh stays in the task's own storage,
    # so the keys copied in at connect vanish with the task and never reach
    # EFS. (Keys made in the workspace then don't persist either; a .ssh
    # already on EFS from an earlier Secrets Manager period is left alone.)
    [ "${triplec_connect:-0}" = 1 ] && names=".claude .claude.json"
    if [ ! -d "$persist" ] || [ -L "$persist" ]; then
        echo "entrypoint: warning — Hybrid storage, but $persist is not mounted; home state will not persist"
        return 0
    fi
    # The access point already makes its root 1000:1000; said again here so
    # claude (Claude Code's own atomic saves, write_claude_json) can always
    # create files beside the persisted ones.
    [ -z "$owner" ] || chown "$owner" "$persist" 2>/dev/null
    for name in $names; do
        link="$home/$name"
        target="$persist/$name"
        if [ -L "$link" ] && [ "$(readlink "$link")" = "$target" ]; then
            continue
        fi
        if [ ! -e "$target" ] && [ ! -L "$target" ]; then
            if [ -e "$link" ] || [ -L "$link" ]; then
                # Staged then renamed: a copy cut short must never look seeded.
                rm -rf "$target.triple-c-seed"
                if ! cp -a "$link" "$target.triple-c-seed" || ! mv "$target.triple-c-seed" "$target"; then
                    rm -rf "$target.triple-c-seed"
                    echo "entrypoint: warning — could not seed $target; ~/$name stays unpersisted"
                    continue
                fi
            elif [ "$name" = .claude.json ]; then
                ( umask 077; printf '{}\n' > "$target" ) || {
                    echo "entrypoint: warning — could not create $target"; continue; }
            else
                mkdir -m 700 "$target" || {
                    echo "entrypoint: warning — could not create $target"; continue; }
            fi
            [ -z "$owner" ] || chown -R "$owner" "$target" 2>/dev/null
        fi
        rm -rf "$link"
        ln -s "$target" "$link" || echo "entrypoint: warning — could not link ~/$name to $target"
        [ -z "$owner" ] || chown -h "$owner" "$link" 2>/dev/null
    done
    if [ "${triplec_connect:-0}" = 1 ]; then
        echo "entrypoint: Hybrid storage — ~/.claude and ~/.claude.json persist on $persist; ~/.ssh does not (credentials are copied in at connect)"
    else
        echo "entrypoint: Hybrid storage — ~/.claude, ~/.claude.json and ~/.ssh persist on $persist"
    fi
}

# The real directory behind a Hybrid ~/.ssh link, printed only when the link
# is exactly the one link_persistent_home makes and neither it nor the mount
# is itself a symlink. Root chowns and chmods under the result, and the
# persisted directory is claude-writable, so root must never be pointed
# anywhere else by it.
aws_hybrid_ssh_dir() {
    local home="$1" want="$triplec_home_mnt/.ssh"
    [ "$(readlink "$home/.ssh")" = "$want" ] || return 1
    [ ! -L "$triplec_home_mnt" ] && [ ! -L "$want" ] && [ -d "$want" ] || return 1
    printf '%s' "$want"
}

# Whether storage mode $1 persists home state (Hybrid, and EFS = Hybrid +
# /workspace on EFS).
aws_storage_links_home() {
    case "$1" in
        hybrid|efs) return 0 ;;
    esac
    return 1
}

# EFS storage: /workspace is the access point itself (uid/gid 1000 enforced,
# root acts as 1000 there). Nothing here creates or chowns it; only says so
# when it is missing, since the files would then not survive a stop.
aws_efs_workspace_check() {
    mountpoint -q "$1" 2>/dev/null \
        || echo "entrypoint: warning — EFS storage, but $1 is not mounted; its files will not persist"
    return 0
}
# <<< aws_home

# ── Corporate CA certificates ───────────────────────────────────────────────
# The host's CA material is bind-mounted read-only at /tmp/.host-ca. Triple-C
# mounts a *directory* as-is and a *single file* as /tmp/.host-ca/<name>.crt,
# so this only ever has to deal with a directory (the file branch below is
# defensive).
#
# Runs before everything that touches the network — the git credential helper,
# ssh-keyscan, and especially the `claude update` at the bottom of entrypoint.sh,
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
# container at creation time instead. (entrypoint.sh forwards them into the cron
# environment file, because cron jobs start from a bare env.)
# >>> ca_install
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
# <<< ca_install

# ── SSH key setup ──────────────────────────────────────────────────────────
# Host SSH dir is mounted read-only at /tmp/.host-ssh and merged into
# /home/claude/.ssh (which lives on the home volume) so we can fix permissions.
#
# This used to `rm -rf ~/.ssh` and copy the host dir over it on every start,
# which silently deleted any key generated or saved inside the container and
# overwrote container edits to config/known_hosts. Now it is a merge:
#   - Files the container created are never touched, even if the host later
#     gains a file of the same name (the host copy is skipped with a warning).
#   - A file previously copied from the host is refreshed when the host copy
#     changes, and removed when the host drops it — but only while it is still
#     byte-identical to what we copied. Once edited in the container it is the
#     container's.
#   - known_hosts is unioned, never replaced (ssh appends to it at runtime).
# `.triple-c-host-files` records "<sha256>\t<relative path>" for every file we
# copied, which is how the two kinds of file are told apart. Without it (first
# start after upgrading from the wipe-and-copy entrypoint) a differing file is
# backed up to `<name>.triple-c-backup` before the host copy replaces it.
#
# The function body between the markers is executed by a unit test in
# app/src-tauri/src/docker/image.rs; keep it self-contained.
# >>> sync_host_ssh
sync_host_ssh() {
    local src="$1" dest="$2"
    local manifest="$dest/.triple-c-host-files"
    local legacy=0 new_manifest f rel target src_sum cur_sum recorded

    _ssh_sum() {
        if [ -L "$1" ]; then
            printf 'link:%s' "$(readlink "$1")" | sha256sum | cut -d' ' -f1
        else
            sha256sum < "$1" | cut -d' ' -f1
        fi
    }

    mkdir -p "$dest"
    [ -f "$manifest" ] || legacy=1
    new_manifest=$(mktemp)

    while IFS= read -r -d '' f; do
        rel="${f#"$src"/}"
        [ "$rel" = "known_hosts" ] && continue
        target="$dest/$rel"
        src_sum=$(_ssh_sum "$f")
        if [ -e "$target" ] || [ -L "$target" ]; then
            cur_sum=$(_ssh_sum "$target")
            if [ "$cur_sum" != "$src_sum" ]; then
                recorded=""
                [ "$legacy" = 0 ] && recorded=$(awk -F'\t' -v r="$rel" '$2 == r { print $1; exit }' "$manifest")
                if [ "$legacy" = 1 ]; then
                    cp -a "$target" "$target.triple-c-backup"
                    echo "entrypoint: ~/.ssh/$rel differs from host copy; kept old version as $rel.triple-c-backup"
                elif [ -z "$recorded" ] || [ "$recorded" != "$cur_sum" ]; then
                    echo "entrypoint: keeping container's ~/.ssh/$rel (host copy not applied)"
                    continue
                fi
                rm -f "$target"
                mkdir -p "$(dirname "$target")"
                cp -a "$f" "$target"
            fi
        else
            mkdir -p "$(dirname "$target")"
            cp -a "$f" "$target"
        fi
        printf '%s\t%s\n' "$src_sum" "$rel" >> "$new_manifest"
    done < <(find "$src" \( -type f -o -type l \) -print0)

    # Drop files the host no longer provides, unless edited in the container.
    if [ "$legacy" = 0 ]; then
        while IFS=$'\t' read -r recorded rel; do
            [ -n "$rel" ] || continue
            if [ ! -e "$src/$rel" ] && [ ! -L "$src/$rel" ] \
                && { [ -e "$dest/$rel" ] || [ -L "$dest/$rel" ]; } \
                && [ "$(_ssh_sum "$dest/$rel")" = "$recorded" ]; then
                rm -f "$dest/$rel"
            fi
        done < "$manifest"
    fi

    if [ -f "$src/known_hosts" ]; then
        cat "$src/known_hosts" >> "$dest/known_hosts"
        sort -u -o "$dest/known_hosts" "$dest/known_hosts"
    fi

    mv "$new_manifest" "$manifest"
    unset -f _ssh_sum
}
# <<< sync_host_ssh

# The host SSH files in $1 merged into ~/.ssh (sync_host_ssh), then the
# modes ssh insists on. On AWS Hybrid ~/.ssh is a link into the EFS mount; the
# steps below then work on the real directory behind it, and only after
# aws_hybrid_ssh_dir has confirmed the link is the one Triple-C made. Any other
# link: nothing is merged. Docker: always /home/claude/.ssh, as before.
# The entrypoint runs this as root before any user process exists;
# triple-c-apply-connect runs it *as the user* (root never walks a ~/.ssh the
# user has been able to plant links in).
# >>> merge_host_ssh
merge_host_ssh() {
    local src="$1" ssh_home_dir=/home/claude/.ssh
    if [ "$triplec_aws" = 1 ] && [ -L "$ssh_home_dir" ]; then
        ssh_home_dir=$(aws_hybrid_ssh_dir /home/claude) || {
            echo "entrypoint: warning — ~/.ssh is a symlink Triple-C did not make; host SSH files not merged"
            ssh_home_dir=""
        }
    fi
    if [ -n "$ssh_home_dir" ] && [ -d "$src" ]; then
        sync_host_ssh "$src" "$ssh_home_dir"
        chown -R claude:claude "$ssh_home_dir"
        chmod 700 "$ssh_home_dir"
        find "$ssh_home_dir" -type f -name "id_*" ! -name "*.pub" -exec chmod 600 {} \;
        find "$ssh_home_dir" -type f -name "*.triple-c-backup" -exec chmod 600 {} \;
        find "$ssh_home_dir" -type f -name "*.pub" -exec chmod 644 {} \;
        if [ -f "$ssh_home_dir/known_hosts" ]; then
            chmod 644 "$ssh_home_dir/known_hosts"
        fi
        if [ -f "$ssh_home_dir/config" ]; then
            chmod 600 "$ssh_home_dir/config"
        fi
    fi
}
# <<< merge_host_ssh

# ── Git credential helper (for HTTPS token) ─────────────────────────────────
# $1 is the token. Callers pass it as a function argument, never on a
# command line (argv is world-readable in /proc).
# >>> git_credentials
install_git_credentials() {
    local token="$1" CRED_FILE="/home/claude/.git-credentials"
    : > "$CRED_FILE"
    chmod 600 "$CRED_FILE"
    chown claude:claude "$CRED_FILE"
    echo "https://oauth2:${token}@github.com" >> "$CRED_FILE"
    echo "https://oauth2:${token}@gitlab.com" >> "$CRED_FILE"
    echo "https://oauth2:${token}@bitbucket.org" >> "$CRED_FILE"
    git config --file /home/claude/.gitconfig credential.helper "store --file=$CRED_FILE"
}
# <<< git_credentials

# ── AWS: clone the project's repositories ───────────────────────────────────
# An AWS workspace has no project folder to mount; its repositories are cloned
# into /workspace/<folder> once the credential helper, git identity and ~/.ssh
# are in place — as `claude`, with prompts off and a per-repo time limit. A
# folder that is already a repository is left alone. Failures are reported and
# never stop the start: a workspace with one repo missing is still a workspace.
#
# In the background, started after the agent (start_clone_repos below): the
# app gives a starting task's agent only seconds to answer, so a clone in the
# way of the agent would fail every start with a large repository. Terminals
# work while the repos arrive. Progress, for the app to show later, is in
# /run/triple-c-clone (world-readable, root-written): `repos` gets a
# "<folder>\tcloning" line as a clone begins and a
# "<folder>\t<cloned|failed|exists>" line when it ends (the last line for a
# folder is its state), and `done` appears when all are finished. The idle
# watcher counts an unfinished clone as activity; triple-c-wip-push skips a
# folder still `cloning`.
# A clone is made in a scratch folder on the same file system,
# /workspace/.triple-c-clone/<folder>, and renamed into /workspace/<folder>
# (atomic) only once it has succeeded. So nothing appears at a final path until
# it is a whole repository, and the clone code never deletes anything at a
# final path — not on failure, not on a later start. With EFS storage
# /workspace outlives the task: whatever is at a final path is kept (a
# repository is skipped as `exists`), and a clone a killed task left in scratch
# is thrown away at the next start (scratch is never the user's). Scratch is
# made, emptied and renamed from as the user; root never walks their tree.
# triple-c-wip-push skips the scratch folder.
# >>> aws_repos
# clone_repos json root [status_dir]
clone_repos() {
    local json="$1" root="$2" status_dir="${3:-}" folder url branch dest tmp err rc
    local limit="${triplec_clone_timeout:-600}"
    local scratch="$root/.triple-c-clone"
    [ -n "$json" ] || return 0
    _clone_status() { [ -z "$status_dir" ] || printf '%s\t%s\n' "$1" "$2" >> "$status_dir/repos"; }
    # Whatever a killed clone left behind.
    _triplec_as_user rm -rf -- "$scratch"
    _triplec_as_user mkdir -m 700 -- "$scratch" \
        || echo "entrypoint: warning — could not create $scratch; clones will fail"
    while IFS= read -r -d '' folder && IFS= read -r -d '' url && IFS= read -r -d '' branch; do
        dest="$root/$folder"
        tmp="$scratch/$folder"
        if [ -e "$dest/.git" ]; then
            echo "entrypoint: /workspace/$folder is already a repository; not cloning"
            _clone_status "$folder" exists
            continue
        fi
        # An empty directory may be cloned over (the rename replaces it, as
        # `git clone` into it did); anything else at the final path is kept.
        if { [ -e "$dest" ] || [ -L "$dest" ]; } \
            && [ -z "$(_triplec_as_user find "$dest" -maxdepth 0 -type d -empty 2>/dev/null)" ]; then
            echo "Triple-C: clone of $folder failed: /workspace/$folder already exists and is not a repository"
            _clone_status "$folder" failed
            continue
        fi
        local args=(clone --quiet)
        [ -n "$branch" ] && args+=(--branch "$branch")
        args+=(-- "$url" "$tmp")
        echo "entrypoint: cloning $folder..."
        _clone_status "$folder" cloning
        # BatchMode is appended to whatever ssh command the user's git config
        # names (core.sshCommand), read as the user, rather than replacing it.
        err=$(_triplec_as_user sh -c '
            limit=$1; shift
            c=${GIT_SSH_COMMAND:-$(git config --get core.sshCommand)}
            export GIT_SSH_COMMAND="${c:-ssh} -o BatchMode=yes" GIT_TERMINAL_PROMPT=0
            exec timeout -k 10 "$limit" git "$@"' _ "$limit" "${args[@]}" 2>&1 </dev/null)
        rc=$?
        if [ "$rc" -eq 0 ]; then
            # -T: never into a folder made at the final path meanwhile; a
            # rename onto anything but an empty directory fails.
            if err=$(_triplec_as_user mv -T -- "$tmp" "$dest" 2>&1); then
                echo "entrypoint: cloned $folder"
                _clone_status "$folder" cloned
                continue
            fi
        else
            [ "$rc" -eq 124 ] || [ "$rc" -eq 137 ] && err="timed out after ${limit}s"
        fi
        _triplec_as_user rm -rf -- "$tmp"
        echo "Triple-C: clone of $folder failed: $(printf '%s' "$err" | tail -n 1)"
        _clone_status "$folder" failed
    done < <(printf '%s' "$json" | python3 -I -c '
import json, sys
def say(msg):
    print("Triple-C: " + msg, file=sys.stderr)
try:
    repos = json.load(sys.stdin)
except ValueError:
    say("TRIPLEC_REPOS is not valid JSON; nothing cloned")
    sys.exit(0)
if not isinstance(repos, list):
    say("TRIPLEC_REPOS is not a JSON array; nothing cloned")
    sys.exit(0)
def text(v):
    if not isinstance(v, str) or "\0" in v or "\t" in v or "\n" in v:
        return False
    try:
        v.encode("utf-8")
    except UnicodeError:
        return False
    return True
for i, r in enumerate(repos):
    if not isinstance(r, dict):
        say("skipping repository %d: not an object" % i)
        continue
    url, branch, folder = r.get("url"), r.get("branch"), r.get("folder")
    if not text(folder) or folder in ("", ".", "..", ".triple-c-clone") or "/" in folder:
        say("skipping repository %d: bad folder %a" % (i, folder))
        continue
    if not text(url) or not url:
        say("skipping %s: bad url" % folder)
        continue
    if branch is not None and not text(branch):
        say("skipping %s: bad branch" % folder)
        continue
    sys.stdout.write("%s\0%s\0%s\0" % (folder, url, branch or ""))
')
    unset -f _clone_status
    return 0
}

# Copy-at-connect (triplec_connect=1): the credentials arrive with the app's
# first connect, when triple-c-apply-connect has installed them and left
# $triplec_run_dir/connected. The clone waits for that, at most
# triplec_connect_wait seconds (the app connects within a minute or two of
# the agent starting, unless it was closed meanwhile); after that it clones
# without them, says so, and leaves `no-credentials` in the status dir $1 so
# the next connect retries what failed once this clone is done.
aws_wait_connect() {
    local status_dir="$1" waited=0
    while [ ! -e "$triplec_run_dir/connected" ]; do
        if [ "$waited" -ge "$triplec_connect_wait" ]; then
            echo "entrypoint: Triple-C has not connected within ${triplec_connect_wait}s — going ahead without the project's credentials (the next connect retries a clone that fails)"
            [ -z "$status_dir" ] || : > "$status_dir/no-credentials"
            return 0
        fi
        sleep 1
        waited=$((waited + 1))
    done
}

# Runs clone_repos in the background, recording progress in $2, then the
# setup script (aws_run, which must see the repositories). The status dir is
# made even with no repositories, so `done` always ends up there. Under
# copy-at-connect the clone first waits for the credentials (above) when there
# are repositories or a setup script.
start_clone_repos() {
    local json="$1" status_dir="$2"
    rm -rf "$status_dir"
    mkdir -m 755 "$status_dir" || status_dir=""
    (
        trap '' HUP
        # Should this chain end early (an error, a signal) with the setup
        # still marked `running`, say `failed`: the idle watcher counts a
        # running setup as busy and would otherwise never stop the workspace.
        trap 'aws_setup_abandoned' EXIT
        trap 'exit 143' TERM
        (
            # `done` however the clone ends (an error, a signal), or the idle
            # watcher would count it as still running forever.
            [ -z "$status_dir" ] || trap ': > "$status_dir/done"' EXIT
            # Wait when something needs the credentials: a repository to
            # clone, or a setup script (it runs right after this and needs the
            # CA, SSH keys and Git credentials the connect installs).
            local needs_connect=0
            case "$json" in ''|'[]') ;; *) needs_connect=1 ;; esac
            [ -f "${triplec_setup_dir:-/nonexistent}/script" ] && needs_connect=1
            if [ "${triplec_connect:-0}" = 1 ] && [ "$needs_connect" = 1 ]; then
                aws_wait_connect "$status_dir"
            fi
            clone_repos "$json" /workspace "$status_dir"
        )
        aws_run_setup
    ) &
}
# <<< aws_repos
