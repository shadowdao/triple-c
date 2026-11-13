//! Where a project's container runs, and the one interface the rest of the app
//! uses to reach inside it.
//!
//! Spec: `docs/superpowers/specs/2026-11-13-aws-ecs-host-design.md` ("Host
//! abstraction"). This is phase 1 of that plan: every operation that happens
//! *inside* a project container — one-shot and attached execs, file transfer,
//! "is it running" — goes through [`ContainerTarget`] instead of naming
//! `docker::exec` or bollard directly. Today the only variant is
//! [`ContainerTarget::Docker`], which delegates to the existing code in
//! `docker/exec.rs` and `docker/container.rs` with no change in behaviour.
//! [`ContainerTarget::Agent`] reaches a container through the in-container
//! `triple-c-agent` instead (`agent.rs`), which is how the AWS host will get in.
//!
//! ## Why an enum and not a trait object
//!
//! The set of hosts is closed and known at compile time, every method is async,
//! and a `dyn` async trait would need boxing on every call (or the
//! `async-trait` crate) for no benefit. Adding a host means adding a variant and
//! letting the compiler point at every `match` that needs an arm.
//!
//! ## What does *not* go through here
//!
//! Things that only make sense for the local Docker host: image build/pull,
//! base-image migration, snapshot commits, volumes, the LiteLLM gateway and STT
//! sidecars, and (until phase 1b) container create/start/stop and the `die`
//! event stream. Those keep calling `crate::docker` directly. The
//! `no_container_interior_bollard_outside_the_host_layer` test pins which files
//! may still do so.

pub mod agent;
pub mod lifecycle;
pub mod sessions;
pub mod watch;

pub use crate::docker::exec::{
    MAX_DROP_BYTES, OUTPUT_LIMIT_MARKER, PROC_NET_OUTPUT_LIMIT,
};
pub use agent::AgentEndpoint;
pub use sessions::ExecSessionManager;

use bytes::Bytes;
use futures_util::Stream;
use std::pin::Pin;
use tokio::io::AsyncWrite;

use crate::docker;
use crate::models::Project;

/// One chunk of output from an exec, already demultiplexed.
///
/// Host-neutral stand-in for bollard's `LogOutput`. With `tty = true` there is
/// no stdout/stderr split and everything arrives as [`ExecChunk::StdOut`] (Docker
/// reports a TTY stream as `Console`, which maps here too — consumers that cared
/// treated the two the same).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecChunk {
    StdOut(Bytes),
    StdErr(Bytes),
    /// stdin echo or anything else a host might surface. Consumers ignore it.
    Other(Bytes),
}

impl ExecChunk {
    pub fn into_bytes(self) -> Bytes {
        match self {
            ExecChunk::StdOut(b) | ExecChunk::StdErr(b) | ExecChunk::Other(b) => b,
        }
    }
}

pub type ExecOutputStream = Pin<Box<dyn Stream<Item = Result<ExecChunk, String>> + Send>>;

/// An exec that has been created and started with its streams attached.
///
/// `input` is a sink that is never read when the exec was opened with
/// `attach_stdin: false`.
pub struct AttachedExec {
    pub exec_id: String,
    pub output: ExecOutputStream,
    pub input: Pin<Box<dyn AsyncWrite + Send>>,
}

/// Everything about an attached exec except where it runs.
///
/// Defaults match what this app has always used: run as `claude` in
/// `/workspace`, no TTY, stdin attached, no extra env.
#[derive(Debug, Clone)]
pub struct ExecSpec {
    pub cmd: Vec<String>,
    pub user: String,
    pub working_dir: Option<String>,
    pub env: Vec<String>,
    pub tty: bool,
    pub attach_stdin: bool,
}

impl ExecSpec {
    pub fn new(cmd: Vec<String>) -> Self {
        Self {
            cmd,
            user: "claude".to_string(),
            working_dir: Some("/workspace".to_string()),
            env: Vec::new(),
            tty: false,
            attach_stdin: true,
        }
    }
    pub fn user(mut self, user: &str) -> Self {
        self.user = user.to_string();
        self
    }
    pub fn working_dir(mut self, dir: Option<&str>) -> Self {
        self.working_dir = dir.map(str::to_string);
        self
    }
    pub fn env(mut self, env: Vec<String>) -> Self {
        self.env = env;
        self
    }
    pub fn tty(mut self, tty: bool) -> Self {
        self.tty = tty;
        self
    }
    pub fn no_stdin(mut self) -> Self {
        self.attach_stdin = false;
        self
    }
}

/// A byte stream of a tar archive read out of a container.
pub type ArchiveStream = Pin<Box<dyn Stream<Item = Result<Bytes, String>> + Send>>;

/// A specific project container on a specific host.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContainerTarget {
    /// A container on the local Docker daemon, by id or name.
    Docker { container_id: String },
    /// A container reached through the in-container `triple-c-agent`
    /// (`host/agent.rs`; protocol in
    /// `docs/superpowers/specs/2026-11-13-triple-c-agent-protocol.md`). Used
    /// by the AWS host (later) and the local agent harness. The host that
    /// runs the container, not this target, owns its lifecycle.
    Agent(AgentEndpoint),
}

impl ContainerTarget {
    pub fn docker(container_id: impl Into<String>) -> Self {
        ContainerTarget::Docker {
            container_id: container_id.into(),
        }
    }

    /// The target for a project's container, if it has one yet.
    pub fn for_project(project: &Project) -> Option<Self> {
        project.container_id.as_deref().map(Self::docker)
    }

    /// The host-specific identifier (a Docker container id today). For logs
    /// and for keying per-container state; never parse it.
    pub fn id(&self) -> &str {
        match self {
            ContainerTarget::Docker { container_id } => container_id,
            ContainerTarget::Agent(ep) => ep.base_url.as_str(),
        }
    }

    // ── state ────────────────────────────────────────────────────────────────

    /// Whether the container is running. An inspect failure (gone, daemon
    /// down) reads as `false`, as `docker::is_container_running` always has.
    pub async fn is_running(&self) -> Result<bool, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::container::is_container_running(container_id).await
            }
            ContainerTarget::Agent(ep) => agent::is_running(ep).await,
        }
    }

    // ── one-shot execs ───────────────────────────────────────────────────────

    /// Run as `claude`, return combined output. Errors on a non-UTF-8-safe
    /// transport failure only; a non-zero exit is *not* an error here.
    pub async fn exec_oneshot(&self, cmd: Vec<String>) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot(container_id, cmd).await
            }
            ContainerTarget::Agent(ep) => agent::exec_oneshot_as(ep, "claude", cmd, Vec::new())
                .await
                .map(|(out, _)| out),
        }
    }

    pub async fn exec_oneshot_env(
        &self,
        cmd: Vec<String>,
        env: Vec<String>,
    ) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot_env(container_id, cmd, env).await
            }
            ContainerTarget::Agent(ep) => agent::exec_oneshot_as(ep, "claude", cmd, env)
                .await
                .map(|(out, _)| out),
        }
    }

    pub async fn exec_oneshot_env_status(
        &self,
        cmd: Vec<String>,
        env: Vec<String>,
    ) -> Result<(String, i64), String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot_env_status(container_id, cmd, env).await
            }
            ContainerTarget::Agent(ep) => agent::exec_oneshot_as(ep, "claude", cmd, env).await,
        }
    }

    pub async fn exec_oneshot_as(
        &self,
        user: &str,
        cmd: Vec<String>,
        env: Vec<String>,
    ) -> Result<(String, i64), String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot_as(container_id, user, cmd, env).await
            }
            ContainerTarget::Agent(ep) => agent::exec_oneshot_as(ep, user, cmd, env).await,
        }
    }

    pub async fn exec_oneshot_limited(
        &self,
        user: &str,
        cmd: Vec<String>,
        limit: usize,
    ) -> Result<(String, i64), String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot_limited(container_id, user, cmd, limit).await
            }
            ContainerTarget::Agent(ep) => {
                agent::exec_oneshot_limited(ep, user, cmd, Vec::new(), limit).await
            }
        }
    }

    /// stdout and stderr kept apart: `(stdout, stderr, exit_code)`.
    pub async fn exec_oneshot_streams_as(
        &self,
        user: &str,
        cmd: Vec<String>,
        env: Vec<String>,
    ) -> Result<(String, String, i64), String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_oneshot_streams_as(container_id, user, cmd, env).await
            }
            ContainerTarget::Agent(ep) => agent::exec_oneshot_streams_as(ep, user, cmd, env).await,
        }
    }

    /// Start a command and return without waiting or reading its output.
    /// Returns the exec id.
    pub async fn exec_detached(&self, spec: ExecSpec) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::exec_detached(container_id, &spec).await
            }
            ContainerTarget::Agent(ep) => agent::exec_detached(ep, &spec).await,
        }
    }

    // ── attached execs ───────────────────────────────────────────────────────

    /// Run as `claude` in `/workspace` with stdin attached.
    pub async fn attach_exec(&self, cmd: Vec<String>, tty: bool) -> Result<AttachedExec, String> {
        self.attach(ExecSpec::new(cmd).tty(tty)).await
    }

    pub async fn attach(&self, spec: ExecSpec) -> Result<AttachedExec, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::create_attached_exec_spec(container_id, &spec).await
            }
            ContainerTarget::Agent(ep) => agent::attach(ep, &spec).await,
        }
    }

    /// The exit code of an exec this target started, once it has finished.
    /// `None` if the host could not say.
    pub async fn wait_exec_exit(&self, exec_id: &str) -> Option<i64> {
        match self {
            ContainerTarget::Docker { .. } => docker::exec::wait_for_exec_exit(exec_id).await,
            ContainerTarget::Agent(ep) => agent::wait_exec_exit(ep, exec_id).await,
        }
    }

    pub async fn resize_exec(&self, exec_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        match self {
            ContainerTarget::Docker { .. } => docker::exec::resize_exec(exec_id, cols, rows).await,
            ContainerTarget::Agent(ep) => agent::resize_exec(ep, exec_id, cols, rows).await,
        }
    }

    // ── files ────────────────────────────────────────────────────────────────

    /// The container user's `(uid, gid)`, falling back to `(1000, 1000)`.
    pub async fn user_ids(&self) -> (u64, u64) {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::container_user_ids(container_id).await
            }
            ContainerTarget::Agent(ep) => agent::user_ids(ep).await,
        }
    }

    /// Write `data` to `dest_dir/file_name` as root-owned with `mode`.
    pub async fn upload_bytes(
        &self,
        dest_dir: &str,
        file_name: &str,
        data: &[u8],
        mode: u32,
    ) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::upload_bytes_to_container(
                    container_id,
                    dest_dir,
                    file_name,
                    data,
                    mode,
                )
                .await
            }
            ContainerTarget::Agent(ep) => {
                agent::upload_bytes_with_ids(ep, dest_dir, file_name, data, mode, (0, 0)).await
            }
        }
    }

    /// [`Self::upload_bytes`] with the owner set to `ids`.
    pub async fn upload_bytes_with_ids(
        &self,
        dest_dir: &str,
        file_name: &str,
        data: &[u8],
        mode: u32,
        ids: (u64, u64),
    ) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::upload_bytes_with_ids(
                    container_id,
                    dest_dir,
                    file_name,
                    data,
                    mode,
                    ids,
                )
                .await
            }
            ContainerTarget::Agent(ep) => {
                agent::upload_bytes_with_ids(ep, dest_dir, file_name, data, mode, ids).await
            }
        }
    }

    /// Copy a host file in, owned by the container user.
    pub async fn upload_host_file(
        &self,
        host_path: &str,
        dest_dir: &str,
        dest_name: &str,
    ) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::upload_host_file_to_container(
                    container_id,
                    host_path,
                    dest_dir,
                    dest_name,
                )
                .await
            }
            ContainerTarget::Agent(ep) => {
                let ids = agent::user_ids(ep).await;
                agent::upload_host_file_with_ids(ep, host_path, dest_dir, dest_name, ids).await
            }
        }
    }

    /// [`Self::upload_host_file`] with the owner ids already known.
    pub async fn upload_host_file_with_ids(
        &self,
        host_path: &str,
        dest_dir: &str,
        dest_name: &str,
        ids: (u64, u64),
    ) -> Result<String, String> {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::upload_host_file_with_ids(
                    container_id,
                    host_path,
                    dest_dir,
                    dest_name,
                    ids,
                )
                .await
            }
            ContainerTarget::Agent(ep) => {
                agent::upload_host_file_with_ids(ep, host_path, dest_dir, dest_name, ids).await
            }
        }
    }

    /// Read `path` out of the container as a tar stream (Docker's archive
    /// API). Dropping the stream cancels the transfer.
    pub fn download_archive(&self, path: &str) -> ArchiveStream {
        match self {
            ContainerTarget::Docker { container_id } => {
                docker::exec::download_archive(container_id, path)
            }
            ContainerTarget::Agent(ep) => agent::download_archive(ep, path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_spec_defaults_match_what_the_app_always_used() {
        let s = ExecSpec::new(vec!["true".into()]);
        assert_eq!(s.user, "claude");
        assert_eq!(s.working_dir.as_deref(), Some("/workspace"));
        assert!(s.env.is_empty());
        assert!(!s.tty);
        assert!(s.attach_stdin);
    }

    #[test]
    fn for_project_follows_the_container_id() {
        let mut p: Project = serde_json::from_value(serde_json::json!({
            "id": "p1", "name": "p", "paths": [], "container_id": null,
            "status": "stopped", "backend": "anthropic",
            "allow_docker_access": false, "full_permissions": false,
            "created_at": "now", "updated_at": "now"
        }))
        .expect("minimal project deserializes");
        assert_eq!(ContainerTarget::for_project(&p), None);
        p.container_id = Some("abc".into());
        assert_eq!(
            ContainerTarget::for_project(&p),
            Some(ContainerTarget::docker("abc"))
        );
        assert_eq!(ContainerTarget::docker("abc").id(), "abc");
    }

    /// Every non-`docker/`, non-`host/` source file, with its path relative to
    /// `src/`, minus the files in `allowed`.
    fn sources_outside_the_host_layer(allowed: &[&str]) -> Vec<(String, String)> {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path
                    .strip_prefix(&src)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if path.is_dir() {
                    if rel != "docker" && rel != "host" {
                        stack.push(path);
                    }
                    continue;
                }
                if rel.ends_with(".rs") && !allowed.contains(&rel.as_str()) {
                    out.push((rel, std::fs::read_to_string(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    /// Docker-only by nature: base-image migration replays apt, restores a
    /// payload, commits and swaps containers. It has no AWS meaning.
    const DOCKER_ONLY: &[&str] = &["commands/migration_commands.rs"];

    /// The host boundary, pinned: outside `docker/` and `host/`, nothing names
    /// bollard or `docker::exec`. Container-interior code goes through
    /// [`ContainerTarget`]. Shrink [`DOCKER_ONLY`]; do not grow it.
    #[test]
    fn no_container_interior_bollard_outside_the_host_layer() {
        let offenders: Vec<String> = sources_outside_the_host_layer(DOCKER_ONLY)
            .into_iter()
            .filter(|(_, text)| {
                text.contains("bollard::")
                    || text.contains("docker::exec")
                    || text.contains("get_docker")
            })
            .map(|(rel, _)| rel)
            .collect();
        assert!(
            offenders.is_empty(),
            "these files reach into the container without going through host::ContainerTarget: {:?}",
            offenders
        );
    }

    /// The lifecycle half of the boundary: outside `docker/` and `host/`, no
    /// non-test code creates, starts, stops, finds or recreates a container
    /// itself (that is [`lifecycle::ProjectHost`]'s job), and none builds a
    /// [`ContainerTarget::docker`] from a bare id, which would hard-code the
    /// host. Comments and `#[cfg(test)]` code are not checked.
    ///
    /// `docker::remove_container` is deliberately absent: startup's
    /// pending-cleanup retry removes Docker leftovers it recorded *by name*,
    /// which is Docker housekeeping, not lifecycle.
    #[test]
    fn no_container_lifecycle_or_bare_docker_targets_outside_the_host_layer() {
        const BANNED: &[&str] = &[
            "docker::create_container(",
            "docker::start_container(",
            "docker::stop_container(",
            "docker::find_existing_container(",
            "docker::container_needs_recreation(",
            "docker::commit_container_snapshot(",
            "docker::remove_snapshot_image(",
            "docker::remove_project_volumes(",
            "docker::is_container_running(",
            "container::is_container_running(",
            "ContainerTarget::docker(",
        ];
        let mut offenders = Vec::new();
        for (rel, text) in sources_outside_the_host_layer(DOCKER_ONLY) {
            let code = text.split("#[cfg(test)]").next().unwrap_or("");
            for (n, line) in code.lines().enumerate() {
                let line = line.trim_start();
                if line.starts_with("//") {
                    continue;
                }
                for pat in BANNED {
                    if line.contains(pat) {
                        offenders.push(format!("{}:{}: {}", rel, n + 1, pat));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "container lifecycle / bare Docker targets outside host::lifecycle:\n{}",
            offenders.join("\n")
        );
    }
}
