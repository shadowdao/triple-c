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
| `TRIPLEC_AGENT_TOKEN` | Bearer token, at least 32 bytes. Removed from the agent's env before anything is spawned. |
| `TRIPLEC_AGENT_TOKEN_FILE` | Read when the env var is unset or empty. Surrounding whitespace is trimmed. |
| `TRIPLEC_AGENT_ADDR` | Listen address. Default `127.0.0.1:47900`. |

Without a token of at least 32 bytes the agent exits with status 2. There is no unauthenticated mode. On
startup it logs a single line, `triple-c-agent <version> listening on <addr>`, to stderr. The token
is never logged.

It must run **as root** (see CLAUDE.md, "Never hand the container's lifetime to a process the
`claude` user owns"). Execs then switch to the requested user. A non-root agent can only run execs
as itself and answers `403` for any other user. The tests rely on that.

## Tests

```bash
cd agent
cargo test                                   # everything, as the current user
cargo clippy --all-targets
# The root-only test (user switch + chown), as root:
sudo -E env "PATH=$PATH" CARGO_HOME="$HOME/.cargo" RUSTUP_HOME="$HOME/.rustup" \
  cargo test --test protocol -- --include-ignored
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

cd agent
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
