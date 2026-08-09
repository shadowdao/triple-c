//! Is there anything in this container worth watching, and can we serve a viewer
//! for it?
//!
//! Playwright is **not** in the container image — it is installed by the user or
//! by Claude, into whichever `node_modules` happens to be in scope. So detection
//! has to be done inside the container, at the moment the pane is opened, and it
//! has to produce an *actionable* answer when the pieces are missing: the pane's
//! one unforgivable failure mode would be an unexplained spinner.
//!
//! Three things must line up:
//!
//! 1. **`playwright-core`** (directly, or via `playwright`, which re-exports it),
//! 2. at a version whose `Browser` exposes **`bind()`** — the live-dashboard API
//!    that publishes a browser for a viewer to attach to, and
//! 3. **`@playwright/cli`**, which ships the viewer UI itself.
//!
//! Discovery of published browsers is local-filesystem based (a cache directory
//! plus a unix-socket singleton in the temp dir), which is exactly why the viewer
//! has to run *in the container* next to the browsers rather than on the host.

use serde::{Deserialize, Serialize};

use crate::docker::exec::exec_oneshot;

/// Marks the JSON payload in the probe's stdout, so unrelated chatter on the
/// same stream (npm notices, Node warnings) can't be mistaken for the result.
const MARKER: &str = "__TRIPLE_C_BROWSER_VIEW__";

/// What the probe found. Serialised straight to the frontend so the pane can
/// explain itself precisely rather than saying "not available".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaywrightDetection {
    /// Node's own version, if `node` ran at all.
    #[serde(default)]
    pub node_version: Option<String>,
    /// Resolved `playwright-core` (or `playwright`) version.
    #[serde(default)]
    pub playwright_version: Option<String>,
    /// Absolute path of the resolved package manifest, for the diagnostics line.
    #[serde(default)]
    pub playwright_path: Option<String>,
    /// Whether the resolved build's type definitions declare `Browser.bind()`.
    #[serde(default)]
    pub has_bind: bool,
    /// Resolved `@playwright/cli` version — the package that serves the viewer.
    #[serde(default)]
    pub cli_version: Option<String>,
    /// Absolute path of `@playwright/cli`'s entry script. Invoked with `node`
    /// directly rather than through its bin shim, so the viewer's PID is the one
    /// we can signal.
    #[serde(default)]
    pub cli_entry: Option<String>,
    /// Where the probe looked, echoed back for the "not found" message.
    #[serde(default)]
    pub searched: Vec<String>,
}

impl PlaywrightDetection {
    /// Everything needed to actually serve the pane.
    pub fn is_usable(&self) -> bool {
        self.playwright_version.is_some() && self.has_bind && self.cli_entry.is_some()
    }

    /// A specific, actionable explanation of what is missing. `None` when the
    /// container is ready.
    pub fn blocker(&self) -> Option<String> {
        if self.node_version.is_none() {
            return Some(
                "Node.js isn't runnable in this container, so Playwright can't be detected."
                    .to_string(),
            );
        }
        if self.playwright_version.is_none() {
            return Some(format!(
                "Playwright isn't installed in this container. Install it with \
                 `npm i -D playwright` (or `npm i -g playwright`), then have Claude call \
                 `await browser.bind('claude')` after launching a browser — or use \
                 `@playwright/mcp`, which binds automatically. Looked in: {}.",
                if self.searched.is_empty() {
                    "the container's default module paths".to_string()
                } else {
                    self.searched.join(", ")
                }
            ));
        }
        if !self.has_bind {
            return Some(format!(
                "Playwright {} is installed, but it predates the live-dashboard API \
                 (`browser.bind()`). Upgrade with `npm i -D playwright@latest` and restart \
                 the browser Claude is driving.",
                self.playwright_version.as_deref().unwrap_or("?")
            ));
        }
        if self.cli_entry.is_none() {
            return Some(
                "Playwright is installed, but the viewer UI package isn't. Install it with \
                 `npm i -D @playwright/cli`, then reopen this tab."
                    .to_string(),
            );
        }
        None
    }
}

/// One `node -e` probe, run as `claude` inside the container.
///
/// No shell quoting is involved: the script is a single `argv` element. The
/// script finds the global `node_modules` root itself, so a Playwright installed
/// with `npm i -g` is found as readily as one in `/workspace/node_modules`.
pub async fn detect(container_id: &str) -> Result<PlaywrightDetection, String> {
    let output = exec_oneshot(
        container_id,
        vec!["node".to_string(), "-e".to_string(), PROBE.to_string()],
    )
    .await?;

    parse_probe_output(&output)
}

/// Pull the marked JSON object out of the probe's combined output.
///
/// `exec_oneshot` interleaves stdout and stderr, and Node happily writes
/// deprecation warnings to the latter, so the payload is located by marker
/// rather than by assuming it is the whole stream.
pub(crate) fn parse_probe_output(output: &str) -> Result<PlaywrightDetection, String> {
    let start = output.find(MARKER).ok_or_else(|| {
        let trimmed = output.trim();
        if trimmed.is_empty() {
            "Playwright detection produced no output. Is Node.js present in the container?"
                .to_string()
        } else {
            format!(
                "Playwright detection failed: {}",
                trimmed.lines().next_back().unwrap_or(trimmed)
            )
        }
    })? + MARKER.len();

    // The payload runs to the end of that line; anything the probe's own
    // children wrote afterwards is not ours.
    let json = output[start..].lines().next().unwrap_or("").trim();
    serde_json::from_str(json)
        .map_err(|e| format!("Could not read the Playwright detection result: {}", e))
}

/// The probe. Kept as one string so the quoting story is "there isn't one".
///
/// Deliberately tolerant: every lookup is individually guarded, because a
/// half-installed `node_modules` must produce a *partial* answer that
/// [`PlaywrightDetection::blocker`] can turn into advice, not an exception that
/// produces "detection failed".
const PROBE: &str = concat!(
    r#"const fs=require("fs"),path=require("path"),cp=require("child_process");"#,
    r#"const out={node_version:process.versions.node,searched:[],has_bind:false};"#,
    // `npm root -g` is the only reliable way to learn the global prefix, and it
    // is cheap enough to pay for once per pane open.
    r#"let g=null;try{g=cp.execSync("npm root -g",{encoding:"utf8",stdio:["ignore","pipe","ignore"]}).trim()||null;}catch(e){}"#,
    r#"const roots=[...new Set(["/workspace",process.cwd(),process.env.HOME?path.join(process.env.HOME,"node_modules"):null,g].filter(Boolean))];"#,
    r#"out.searched=roots;"#,
    r#"const res=(s)=>{for(const r of roots){try{return require.resolve(s,{paths:[r]});}catch(e){}}return null;};"#,
    r#"const core=res("playwright-core/package.json")||res("playwright/package.json");"#,
    r#"if(core){try{out.playwright_path=core;out.playwright_version=JSON.parse(fs.readFileSync(core,"utf8")).version;}catch(e){}"#,
    // `bind`/`unbind` are checked against the shipped type definitions rather
    // than by loading the module: it is a static read, needs no browser, and
    // cannot be tripped up by a package that fails to import.
    r#"try{const t=fs.readFileSync(path.join(path.dirname(core),"types","types.d.ts"),"utf8");"#,
    r#"out.has_bind=/\bunbind\s*\(\s*\)/.test(t)&&/\bbind\s*\(/.test(t);}catch(e){}}"#,
    r#"const cli=res("@playwright/cli/package.json");"#,
    r#"if(cli){try{const j=JSON.parse(fs.readFileSync(cli,"utf8"));out.cli_version=j.version;"#,
    r#"const b=typeof j.bin==="string"?{[j.name]:j.bin}:(j.bin||{});const k=Object.keys(b)[0];"#,
    r#"if(k)out.cli_entry=path.resolve(path.dirname(cli),b[k]);}catch(e){}}"#,
    r#"process.stdout.write("\n__TRIPLE_C_BROWSER_VIEW__"+JSON.stringify(out)+"\n");"#,
);

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(json: &str) -> String {
        format!("some npm noise\n{}{}\n", MARKER, json)
    }

    #[test]
    fn a_complete_install_is_usable() {
        let d = parse_probe_output(&payload(
            r#"{"node_version":"22.11.0","playwright_version":"1.62.1","has_bind":true,"cli_version":"0.1.18","cli_entry":"/workspace/node_modules/@playwright/cli/playwright-cli.js","searched":["/workspace"]}"#,
        ))
        .unwrap();
        assert!(d.is_usable());
        assert_eq!(d.blocker(), None);
    }

    #[test]
    fn stderr_noise_before_and_after_the_payload_is_ignored() {
        let out = format!(
            "(node:41) Warning: something\n{}{}\nnpm notice trailing\n",
            MARKER, r#"{"node_version":"22.11.0","has_bind":false}"#
        );
        let d = parse_probe_output(&out).unwrap();
        assert_eq!(d.node_version.as_deref(), Some("22.11.0"));
    }

    #[test]
    fn a_missing_playwright_is_reported_with_where_we_looked() {
        let d = parse_probe_output(&payload(
            r#"{"node_version":"22.11.0","searched":["/workspace","/usr/lib/node_modules"]}"#,
        ))
        .unwrap();
        assert!(!d.is_usable());
        let msg = d.blocker().unwrap();
        assert!(msg.contains("npm i -D playwright"), "{}", msg);
        assert!(msg.contains("browser.bind"), "{}", msg);
        assert!(msg.contains("/usr/lib/node_modules"), "{}", msg);
    }

    #[test]
    fn a_playwright_without_bind_asks_for_an_upgrade() {
        let d = parse_probe_output(&payload(
            r#"{"node_version":"22.11.0","playwright_version":"1.44.0","has_bind":false,"cli_entry":"/x/cli.js"}"#,
        ))
        .unwrap();
        let msg = d.blocker().unwrap();
        assert!(msg.contains("1.44.0"), "{}", msg);
        assert!(msg.contains("playwright@latest"), "{}", msg);
    }

    #[test]
    fn a_missing_viewer_package_is_reported_separately() {
        let d = parse_probe_output(&payload(
            r#"{"node_version":"22.11.0","playwright_version":"1.62.1","has_bind":true}"#,
        ))
        .unwrap();
        assert!(!d.is_usable());
        assert!(d.blocker().unwrap().contains("@playwright/cli"));
    }

    #[test]
    fn a_container_without_node_says_so() {
        let d = parse_probe_output(&payload(r#"{"has_bind":false}"#)).unwrap();
        assert!(d.blocker().unwrap().contains("Node.js"));
    }

    #[test]
    fn an_unmarked_stream_surfaces_the_containers_own_error() {
        let err = parse_probe_output("sh: 1: node: not found\n").unwrap_err();
        assert!(err.contains("node: not found"), "{}", err);
    }

    #[test]
    fn an_empty_stream_is_explained_rather_than_parsed() {
        let err = parse_probe_output("   \n").unwrap_err();
        assert!(err.contains("no output"), "{}", err);
    }

    #[test]
    fn the_probe_is_a_single_argv_element_with_no_quoting_hazards() {
        // It is passed straight to `node -e`; a stray single quote would only
        // matter if someone later routed it through a shell, and a newline
        // would break the marker-line contract in `parse_probe_output`.
        assert!(!PROBE.contains('\n'));
        assert!(PROBE.contains(MARKER));
    }
}
