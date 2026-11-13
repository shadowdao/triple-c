# triple-c-agent

The in-container agent: a small static binary that does over authenticated HTTP/WebSocket what
`host::ContainerTarget` does through the Docker API. It provides one-shot, attached and detached
execs, resize, exit status, file upload, archive download and TCP tunnels. The wire protocol is
`docs/superpowers/specs/2026-11-13-triple-c-agent-protocol.md`, and that spec is the contract.
This crate implements it and does not extend it.

It is a standalone crate: not part of any workspace, with its own `Cargo.lock`.

## Running

| Variable | Meaning |
|---|---|
| `TRIPLEC_AGENT_TOKEN` | Bearer token, at least 32 bytes. Removed from the agent's env before anything is spawned, so no exec inherits it — but see below. |
| `TRIPLEC_AGENT_TOKEN_FILE` | Read when the env var is unset or empty. Must be owned by the agent's uid and not readable by others. Surrounding whitespace is trimmed. **Preferred.** |
| `TRIPLEC_AGENT_ADDR` | Listen address. Default `127.0.0.1:47900`. |

Removing `TRIPLEC_AGENT_TOKEN` only edits the process's libc environment. The kernel keeps the
environment the agent was started with, so `/proc/<agent pid>/environ` still shows the token to root
and to any process running as the agent's uid. Use `TRIPLEC_AGENT_TOKEN_FILE`; the entrypoint does.

Connections are HTTP/1.1. A client gets 10 seconds to send a complete request head, and at most 256
connections are served at once; past that, new connections wait in the listen backlog for a slot.
WebSocket sessions (attach, tunnel) stop counting once upgraded, and accept messages of at most
1 MiB.

Execs start with every signal at its default disposition and an empty signal mask, whatever the
agent itself inherited (the entrypoint's supervisor ignores `SIGHUP`), and their stdin/stdout/stderr
are owned by the exec's user unless they are `/dev/null`, as runc arranges for `docker exec`.

Without a token of at least 32 bytes the agent exits with status 2. There is no unauthenticated mode. On
startup it logs a single line, `triple-c-agent <version> listening on <addr>`, to stderr. The token
is never logged.

It must run **as root** (see CLAUDE.md, "Never hand the container's lifetime to a process the
`claude` user owns"). Execs then switch to the requested user. A non-root agent can only run execs
as itself and answers `403` for any other user. The tests rely on that.

## Tests

```bash
cd container/triple-c-agent
cargo test                                   # everything, as the current user
cargo clippy --all-targets
# The root-only tests (user switch, upload chown, stdio ownership), as root:
sudo -E env "PATH=$PATH" CARGO_HOME="$HOME/.cargo" RUSTUP_HOME="$HOME/.rustup" \
  cargo test -- --include-ignored
sudo chown -R "$(id -u):$(id -g)" target    # sudo leaves root-owned build files behind
```

The integration tests (`tests/protocol.rs`) start the real server in-process on `127.0.0.1:0` and
drive every endpoint over real HTTP and WebSocket.

## Static release builds

Both targets build from an x86_64 Linux host with **no C cross toolchain**: every dependency is
pure Rust, and Rust's musl targets ship a self-contained CRT. aarch64 links with the `rust-lld`
that comes with the toolchain. Verified with rustc 1.98.0:

```bash
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl

cd container/triple-c-agent
cargo build --release --target x86_64-unknown-linux-musl
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
  cargo build --release --target aarch64-unknown-linux-musl
```

Output:

- `target/x86_64-unknown-linux-musl/release/triple-c-agent`: `static-pie linked`, about 1.3 MB.
- `target/aarch64-unknown-linux-musl/release/triple-c-agent`: `statically linked`, about 1.2 MB.

Check a build with `file <binary>`. It must say `statically linked` or `static-pie linked`, and never
`dynamically linked` or `interpreter`.

Do not add a dependency that needs a C compiler (`ring`, `openssl-sys`, anything using `cc`). The
aarch64 build would then need a cross C toolchain or `cargo-zigbuild`. TLS is out of scope for v1
anyway, because the transport is the SSM channel or loopback.

In the image, `container/Dockerfile`'s `agent-build` stage does this on the build platform for
`$TARGETARCH`; the app's local build passes those build args itself (`docker/image.rs`).

## Docker parity test

The requirement is that every operation through the agent comes back exactly as it does through
`docker exec`. `app/src-tauri/src/host/agent/live_tests.rs` checks that in one real container,
running each operation through both `ContainerTarget::Docker` and `ContainerTarget::Agent` and
comparing. It needs an image whose entrypoint starts the agent. To build one from the published
sandbox image plus this branch's agent and entrypoint:

```bash
cargo build --release --target x86_64-unknown-linux-musl     # or aarch64, on an arm64 host
ctx=$(mktemp -d)
cp target/x86_64-unknown-linux-musl/release/triple-c-agent ../entrypoint.sh "$ctx"/
cat > "$ctx/Dockerfile" <<'DOCKERFILE'
FROM ghcr.io/shadowdao/triple-c-sandbox:latest
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
COPY triple-c-agent /usr/local/bin/triple-c-agent
RUN chmod 0755 /usr/local/bin/entrypoint.sh /usr/local/bin/triple-c-agent
DOCKERFILE
docker build -t tc-agent-e2e:latest "$ctx"

cd ../../app/src-tauri
cargo test --lib the_agent_answers_exactly_as_docker_exec_does -- --ignored --nocapture
```

The test starts and removes its own container. It reaches the agent on the published loopback port
when run on the host, or on the container's bridge address when run from inside another container
on the same daemon. It has already caught one real divergence: `docker exec` sets only `HOME` for
the exec's user, never `USER`/`LOGNAME`.
