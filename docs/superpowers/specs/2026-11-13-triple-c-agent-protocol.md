# triple-c-agent — wire protocol (v1)

Date: 2026-11-13
Status: draft, phase 2 of `2026-11-13-aws-ecs-host-design.md` ("Connectivity")

## Purpose

`triple-c-agent` is a small static binary inside the project container. On AWS, the app reaches
it through one SSM port-forwarding session. Locally it can be reached through a published
loopback port, which is how it is tested. It does, over HTTP, everything `host::ContainerTarget`
does through the Docker API today: one-shot, attached and detached execs, resize, exit status,
file upload, archive download, and TCP tunnels to in-container loopback ports.

**Docker parity is the requirement.** The same `ExecSpec` must behave the same through the agent
as through `docker exec`: same user, working directory, environment and output split. Where Docker
has a quirk the app relies on, the agent reproduces it, and this document names it.

## Process

- **Binary:** `/usr/local/bin/triple-c-agent`, built from `agent/` in the repo, static
  (`*-unknown-linux-musl`), for amd64 and arm64.
- **Runs as root**, started by `entrypoint.sh` before the keepalive, so a `claude`-user
  `pkill`/`kill -9 -1` can't kill it (the same rule as the keepalive; see CLAUDE.md).
- **Listens on** `TRIPLEC_AGENT_ADDR`, default `127.0.0.1:47900`. It never binds a non-loopback
  address unless that address is given explicitly. The local test harness publishes the port to
  the host's loopback.
- **Token:** `TRIPLEC_AGENT_TOKEN` (env) or the file at `TRIPLEC_AGENT_TOKEN_FILE`. If neither
  gives a non-empty token of at least 32 bytes, **the agent refuses to start** — there is no
  unauthenticated mode. After reading the token, the agent removes `TRIPLEC_AGENT_TOKEN` from its
  own environment, so execs don't inherit it.
- **Base environment for execs:** the agent's own environment at startup (minus the token), which
  is the container's configured env — the same thing `docker exec` inherits.
- **Default working directory:** the agent's cwd at startup (the container's `WorkingDir`), used
  when a spec gives `working_dir: null`. This matches `docker exec` without `-w`.

## Common rules

- **Every request** needs `Authorization: Bearer <token>`. A missing or wrong token gets `401`
  with an empty body. The comparison is constant-time.
- **Errors** are `4xx`/`5xx` with body `{"error": "<message>"}`, Content-Type `application/json`.
- **JSON** is UTF-8. Raw bytes inside JSON are standard base64 with padding.
- **Users** are names resolved through `/etc/passwd` and `/etc/group` in the container. An exec
  runs with the user's uid, primary gid and supplementary groups (`initgroups`). `HOME`, `USER`
  and `LOGNAME` are set from passwd **unless the spec's env already sets them**. When the agent is
  not root (tests), a spec naming a user other than the agent's own fails with `403`.
- **ExecSpec** (JSON):
  ```json
  {"cmd": ["sh","-c","…"], "user": "claude", "working_dir": "/workspace" | null,
   "env": ["K=V", …], "tty": false}
  ```
  `cmd` must be non-empty. `env` entries are `K=V`; later entries win, and they override the base
  environment. `cmd[0]` is resolved through `PATH` from the **merged** environment.

## Endpoints

### `GET /v1/health`
`200 {"agent": "triple-c-agent", "version": "<semver>", "protocol": 1}`.

### `POST /v1/exec` — one-shot
Body: an ExecSpec plus `"limit": <bytes>`. stdin is `/dev/null`, there is no tty, and the agent
waits for exit.

`200 {"chunks": [{"s": "out"|"err", "d": "<base64>"}, …], "exit_code": <int>}`

- `chunks` are in **arrival order**. The client rebuilds the combined text, stdout only, and
  stderr only from them, which is exactly what `docker/exec.rs`'s `OneshotOutput` does.
- When the combined size would exceed `limit`, the agent kills the process group and returns
  `413 {"error": "OUTPUT_LIMIT: Command output exceeded <limit> bytes and was abandoned"}`. The
  client passes that message through unchanged, so `OUTPUT_LIMIT_MARKER` checks keep working.
- A process killed by a signal reports `exit_code` 128 + signal, as Docker does.
- If the command can't be started (no such binary or user), the response is `200` with
  `exit_code` 126 or 127 and the error text in one `err` chunk. This mirrors what a shell and
  Docker report.

### `POST /v1/exec/detached`
Body: an ExecSpec. Starts the command with stdin, stdout and stderr all on `/dev/null`.
`200 {"exec_id": "<id>"}`.

### `GET /v1/exec/{id}`
`200 {"running": bool, "exit_code": <int> | null}`. Ids come from `detached` and `attach`. The
agent keeps finished entries for at least 10 minutes. An unknown id gets `404`.

### `POST /v1/exec/{id}/resize`
Body: `{"cols": u16, "rows": u16}`. `204`. Allowed only for tty execs; anything else gets `409`.

### `GET /v1/attach` — WebSocket
1. The client sends one **text** frame: an ExecSpec plus `"attach_stdin": bool`, and for tty
   execs optionally `"cols"` and `"rows"`.
2. The agent replies with **text** `{"type": "started", "exec_id": "<id>"}`, or with
   `{"type": "error", "error": "…"}` followed by a close.
3. **Agent → client binary frames:** byte 0 is the channel (`1` = stdout, `2` = stderr) and the
   rest is payload. With `tty: true` the command runs on a pty and everything is channel `1`
   (Docker's `Console` maps to stdout).
4. **Client → agent binary frames:** stdin bytes. These are ignored when `attach_stdin` is false;
   in that case stdin is `/dev/null`.
5. **Client → agent text** `{"type": "close_stdin"}`: closes the child's stdin (EOF). Without a
   tty, closing the WebSocket's write side has the same effect.
6. When the process exits and all output has been sent, the agent sends **text**
   `{"type": "exit", "code": <int>}` and closes. The exit code is also kept for
   `GET /v1/exec/{id}`.
7. If the client goes away, the agent sends SIGHUP to the process group (tty) or closes stdin
   (non-tty), as a dropped `docker exec` does. **It does not kill a non-tty process**: Docker
   doesn't either, and `gh_login`/`setup-token` clean up with their own `pkill`.

### `PUT /v1/files?dir=<abs>&name=<name>&mode=<octal>&uid=<n>&gid=<n>`
The body is the raw file bytes, at most 256 MiB plus 1 (`MAX_DROP_BYTES`). Writes
`dir/name` with `mode`, owned by `uid:gid`, mtime now, replacing an existing file the way tar
extraction does (unlink, then create). `dir` must exist; it is not created. `name` must be a
single path component. `200 {"path": "<dir>/<name>"}`.

### `GET /v1/archive?path=<abs>`
`200 application/x-tar`, streamed: a tar of `path`, whose root member is `basename(path)`, with
recursion for directories. This matches Docker's `GET /containers/{id}/archive`. A missing path
gets `404`. Dropping the connection cancels the read.

### `GET /v1/tunnel?port=<n>` — WebSocket
The agent connects to `127.0.0.1:<n>` **inside the container** and pipes binary frames both ways.
If the connection is refused, the agent sends text `{"type": "error", "error": "…"}` and closes.
When either side ends, the other is closed.

### `GET /v1/activity`
`200 {"last_activity_unix": <int>, "active_execs": <int>, "active_tunnels": <int>}`.
"Activity" means any request other than `health` and `activity`, or any byte through an attach or
tunnel. This is used by idle auto-stop later.

## Client mapping (`host::ContainerTarget::Agent`)

| ContainerTarget | Agent call |
|---|---|
| `exec_oneshot*` (all variants) | `POST /v1/exec`, with `limit` = the variant's limit (`MAX_ONESHOT_OUTPUT` or explicit) |
| `exec_detached` | `POST /v1/exec/detached` |
| `attach` / `attach_exec` | `GET /v1/attach`; `input` is an `AsyncWrite` whose shutdown sends `close_stdin` |
| `wait_exec_exit` | poll `GET /v1/exec/{id}` (same budget as the Docker version: 200 × 25 ms, shared constants in `docker/exec.rs`) |
| `resize_exec` | `POST /v1/exec/{id}/resize` |
| `user_ids` | one-shot `sh -c 'id -u; id -g'` as `claude`, falling back to `(1000, 1000)` |
| `upload_bytes*`, `upload_host_file*` | `PUT /v1/files`; a multi-component `name` (the terminal drop's `triple-c-drops/<file>`) is split into `dir` + last component, `.`/`..` refused |
| `download_archive` | `GET /v1/archive` |
| `is_running` | `GET /v1/health` succeeds → `true`; a connection error → `false` |
| `has_exited` | `false` (only the host can say positively; an AWS host answers from ECS) |
| (future) tunnels | `GET /v1/tunnel` |

## Not in v1

TLS (the transport is the SSM channel or loopback), multiplexing within one connection (the SSM
port-forward multiplexes TCP connections), filesystem watch, and auth rotation while running.
