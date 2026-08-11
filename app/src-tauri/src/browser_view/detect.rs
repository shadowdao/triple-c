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
//!
//! ## Where a Playwright can legitimately be
//!
//! `node_modules` is not the only answer, and assuming it was is what made this
//! probe lie. `claude mcp add … npx @playwright/mcp@latest` — the way most
//! people end up with Playwright in the container — installs nothing into any
//! `node_modules`: npx unpacks the tree into `~/.npm/_npx/<hash>/node_modules`
//! and runs it from there. So that cache is searched too, every entry of it,
//! and [`PlaywrightDetection::searched`] echoes back every root actually
//! consulted so a "not found" is checkable rather than merely asserted.
//!
//! Note what that npx route can and cannot do: `@playwright/mcp` bundles a
//! `playwright-core` new enough to `bind()`, so it can satisfy points 1 and 2 —
//! but it never ships `@playwright/cli`, so it can never satisfy point 3 on its
//! own. Any message that offers it as a way to *set up* this pane is sending
//! the user down a dead end; see [`PlaywrightDetection::blocker`].

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
    /// Absolute path of the resolved Playwright's own CLI entry (`cli.js`).
    ///
    /// Both `playwright` and `playwright-core` declare one, and it is the thing
    /// that installs browsers and their system libraries. Driving *that* file
    /// with `node` — rather than whatever `playwright` happens to be on `PATH` —
    /// is what keeps the browser install pinned to the copy this pane found.
    #[serde(default)]
    pub playwright_cli: Option<String>,
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
    /// Browser bundles present in the Playwright browser cache
    /// (`~/.cache/ms-playwright`), e.g. `chromium-1200`. `ffmpeg-*` is excluded
    /// — it is not a browser and its presence must not read as one.
    ///
    /// Not part of [`PlaywrightDetection::is_usable`]: the viewer serves
    /// whatever has been published to it, and a browser could in principle be
    /// remote. It is here because "installed but no browser to drive" is a real
    /// state the pane has to be able to say out loud.
    #[serde(default)]
    pub browsers: Vec<String>,
    /// Path to Google Chrome, if the `chrome` *channel* is installed.
    ///
    /// Separate from [`Self::browsers`] because it is not in Playwright's cache
    /// at all — the channel is an apt package. It is tracked because
    /// `@playwright/mcp` asks for `channel: 'chrome'` specifically, so a
    /// container with the bundled Chromium and no Chrome is set up for the
    /// user's own scripts and not for the MCP plugin.
    #[serde(default)]
    pub chrome_channel: Option<String>,
    /// The Chromium binary the *resolved* Playwright would launch, asked of the
    /// build itself rather than derived from the cache listing.
    #[serde(default)]
    pub chromium_executable: Option<String>,
    /// Whether that binary is actually on disk.
    ///
    /// False with a non-empty [`Self::browsers`] is the revision-skew case: two
    /// Playwright copies in one container pin different revisions, so the cache
    /// can be full of browsers and every launch still fail.
    #[serde(default)]
    pub chromium_executable_exists: bool,
    /// The version a *script's* `require("playwright")` resolves to.
    ///
    /// Tracked separately from [`Self::playwright_version`] because they are
    /// routinely different in one directory: `@playwright/cli` pins its own
    /// `playwright-core`, npm hoists that, and a separately-installed
    /// `playwright` then nests a second core beside it. The viewer uses one,
    /// Claude's scripts use the other.
    #[serde(default)]
    pub script_playwright_version: Option<String>,
    /// The Chromium that copy would launch, and whether it is there. This is
    /// the pair that decides whether a script Claude writes actually runs.
    #[serde(default)]
    pub script_chromium_executable: Option<String>,
    #[serde(default)]
    pub script_chromium_executable_exists: bool,
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
    ///
    /// Every branch names the *package* that is missing and points at this
    /// pane's install action, because assembling npm commands by hand is the
    /// thing that went wrong for real users. `@playwright/mcp` is named only in
    /// the role it actually plays — it binds sessions automatically once
    /// Playwright is present — and never as a route through setup, because it
    /// does not ship `@playwright/cli` and so can never make the viewer work.
    pub fn blocker(&self) -> Option<String> {
        if self.node_version.is_none() {
            return Some(
                "Node.js isn't runnable in this container, so Playwright can't be detected."
                    .to_string(),
            );
        }
        if self.playwright_version.is_none() {
            return Some(format!(
                "Playwright isn't installed in this container. Two packages are needed: \
                 `playwright` (for the `browser.bind()` live-dashboard API) and \
                 `@playwright/cli` (the viewer UI this pane embeds). Use “Set up Playwright” \
                 below to install both into the container. Installing `@playwright/mcp` on \
                 its own is not enough — it binds sessions for you once Playwright is there, \
                 but it never provides the viewer. Looked in: {}.",
                self.searched_text()
            ));
        }
        if !self.has_bind {
            return Some(format!(
                "Playwright {} is installed{}, but it predates the live-dashboard API \
                 (`browser.bind()`). Use “Set up Playwright” below to upgrade to the latest \
                 `playwright`, then restart the browser Claude is driving.",
                self.playwright_version.as_deref().unwrap_or("?"),
                match self.playwright_path.as_deref() {
                    Some(p) => format!(" at {}", p),
                    None => String::new(),
                }
            ));
        }
        if self.cli_entry.is_none() {
            return Some(format!(
                "Playwright {} is installed, but `@playwright/cli` — the package that serves \
                 the viewer UI — isn't, and nothing else provides it (`@playwright/mcp` does \
                 not). Use “Set up Playwright” below to install it. Looked in: {}.",
                self.playwright_version.as_deref().unwrap_or("?"),
                self.searched_text()
            ));
        }
        None
    }

    /// The revision-skew sentence, for the pane's browser step.
    ///
    /// Separate from [`Self::blocker`] because it does not block the *viewer* —
    /// the dashboard runs fine; it is the browser that cannot start. Names both
    /// halves, because "install a browser" over a cache that visibly already
    /// has one reads as nonsense without them.
    pub fn skew_message(&self) -> Option<String> {
        if !self.revision_skew() {
            return None;
        }
        // Which half is broken changes what the user sees, so say the one that
        // is. The scripts case is the one that looks like a lie: the pane is
        // green, the viewer works, and every script Claude writes dies.
        if self.scripts_cannot_launch() {
            return Some(format!(
                "This container has {}, and the viewer works — but `require(\"playwright\")` \
                 resolves Playwright {}, which launches {}. That file isn't there, so every \
                 script Claude writes fails with “Executable doesn't exist”. Two copies ended \
                 up in one tree: `@playwright/cli` pins its own `playwright-core`, and a \
                 separately-installed `playwright` nests a second one beside it. “Set up \
                 Playwright” below reinstalls them as one consistent set.",
                self.browsers.join(", "),
                self.script_playwright_version.as_deref().unwrap_or("?"),
                self.script_chromium_executable.as_deref().unwrap_or("?"),
            ));
        }
        Some(format!(
            "This container has {}, but Playwright {} launches {} — which isn't there, so \
             every `chromium.launch()` fails with “Executable doesn't exist”. That happens \
             when two Playwright copies share a container (typically an npx `@playwright/mcp` \
             alongside this one); each pins its own browser revision. “Install Chromium” below \
             fetches the revision this build needs — it runs that build's own installer, so it \
             cannot pick the wrong one again.",
            self.browsers.join(", "),
            self.playwright_version.as_deref().unwrap_or("?"),
            self.chromium_executable.as_deref().unwrap_or("?"),
        ))
    }

    /// Whether Playwright is present but has no browser at all to drive —
    /// neither a downloaded bundle nor the Chrome channel. Advisory: the viewer
    /// still runs, it just has nothing to show until a browser is bound.
    pub fn needs_browser(&self) -> bool {
        self.playwright_version.is_some()
            && self.chrome_channel.is_none()
            && (self.browsers.is_empty() || self.revision_skew())
    }

    /// Browsers are installed, but not the revision this Playwright launches.
    ///
    /// The container looks equipped and every `chromium.launch()` fails with
    /// "Executable doesn't exist". It happens whenever two Playwright copies
    /// share a container — the npx `@playwright/mcp` one and a `/workspace`
    /// one — because each pins its own revision and installs into the same
    /// cache. The install action fixes it: it runs the *resolved* build's own
    /// CLI, so it fetches exactly the revision that was missing.
    ///
    /// Requires the probe to have answered: an older container image, or a
    /// Playwright too broken to `require`, leaves `chromium_executable` unset,
    /// and "didn't answer" must not read as "skewed".
    pub fn revision_skew(&self) -> bool {
        !self.browsers.is_empty() && (self.viewer_cannot_launch() || self.scripts_cannot_launch())
    }

    /// The copy serving the viewer would not find its browser.
    fn viewer_cannot_launch(&self) -> bool {
        self.chromium_executable.is_some() && !self.chromium_executable_exists
    }

    /// `require("playwright")` — what every script Claude writes uses — would
    /// not find its browser. Independent of the above, and the more common of
    /// the two: `@playwright/cli` pins a `playwright-core`, npm hoists it, and
    /// a separately-installed `playwright` nests a second one that no browser
    /// was ever downloaded for.
    fn scripts_cannot_launch(&self) -> bool {
        self.script_chromium_executable.is_some() && !self.script_chromium_executable_exists
    }

    /// The searched roots as prose, so a message never trails off into "Looked
    /// in: ." when the probe couldn't build a root list at all.
    fn searched_text(&self) -> String {
        if self.searched.is_empty() {
            "the container's default module paths".to_string()
        } else {
            self.searched.join(", ")
        }
    }
}

/// One `node -e` probe, run as `claude` inside the container.
///
/// No shell quoting is involved: the script is a single `argv` element. The
/// script finds the global `node_modules` root and the npx cache itself, so a
/// Playwright installed with `npm i -g`, or merely *run* once through
/// `npx @playwright/mcp`, is found as readily as one in
/// `/workspace/node_modules`.
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
    r#"const out={node_version:process.versions.node,searched:[],has_bind:false,browsers:[]};"#,
    // `npm root -g` is the only reliable way to learn the global prefix, and it
    // is cheap enough to pay for once per pane open.
    r#"let g=null;try{g=cp.execSync("npm root -g",{encoding:"utf8",stdio:["ignore","pipe","ignore"]}).trim()||null;}catch(e){}"#,
    r#"const home=process.env.HOME||null;"#,
    // The npx cache. `npm config get cache` would be authoritative but costs a
    // second npm start-up; npm exports its resolved config into the
    // environment of anything it runs, so `npm_config_cache` covers the
    // overridden case and `~/.npm` covers the default.
    r#"const cache=process.env.npm_config_cache||(home?path.join(home,".npm"):null);"#,
    // Every `_npx/<hash>` is a separate tree — `@playwright/mcp` and any other
    // npx-run package each get their own — so all of them are searched, in a
    // stable order, and all of them are reported in `searched`.
    r#"const npx=[];if(cache){try{for(const d of fs.readdirSync(path.join(cache,"_npx")).sort()){"#,
    r#"const p=path.join(cache,"_npx",d,"node_modules");"#,
    r#"try{if(fs.statSync(p).isDirectory())npx.push(p);}catch(e){}}}catch(e){}}"#,
    r#"const roots=[...new Set(["/workspace",process.cwd(),home?path.join(home,"node_modules"):null,g,...npx].filter(Boolean))];"#,
    r#"out.searched=roots;"#,
    r#"const at=(s,r)=>{try{return require.resolve(s,{paths:[r]});}catch(e){return null;}};"#,
    r#"const res=(s)=>{for(const r of roots){const p=at(s,r);if(p)return p;}return null;};"#,
    // One `bin` reader for both packages: `bin` is a string for some manifests
    // and an object for others, and getting that wrong on either one loses the
    // entry point silently.
    r#"const bin=(m,j)=>{const b=typeof j.bin==="string"?{[j.name]:j.bin}:(j.bin||{});"#,
    r#"const k=Object.keys(b)[0];return k?path.resolve(path.dirname(m),b[k]):null;};"#,
    // `playwright-core` is what carries the typings and the browser registry, but
    // it is frequently *nested*: verified against a real `npm i -g playwright
    // @playwright/cli`, npm does not hoist for global installs, so the global
    // root holds `playwright/` and `@playwright/cli/` and no top-level
    // `playwright-core/`. Resolving only the outer `playwright` would then read
    // a package that ships no `types/types.d.ts` at all and report a perfectly
    // current build as "predates browser.bind()". So: hop from the wrapper to
    // its own `playwright-core`, and only fall back to the wrapper's manifest.
    r#"let core=res("playwright-core/package.json");"#,
    r#"if(!core){const pw=res("playwright/package.json");"#,
    r#"if(pw)core=at("playwright-core/package.json",path.dirname(pw))||pw;}"#,
    r#"if(core){try{out.playwright_path=core;const j=JSON.parse(fs.readFileSync(core,"utf8"));"#,
    r#"out.playwright_version=j.version;out.playwright_cli=bin(core,j);}catch(e){}"#,
    // `bind`/`unbind` are checked against the shipped type definitions rather
    // than by loading the module: it is a static read, needs no browser, and
    // cannot be tripped up by a package that fails to import.
    r#"try{const t=fs.readFileSync(path.join(path.dirname(core),"types","types.d.ts"),"utf8");"#,
    r#"out.has_bind=/\bunbind\s*\(\s*\)/.test(t)&&/\bbind\s*\(/.test(t);}catch(e){}}"#,
    r#"const cli=res("@playwright/cli/package.json");"#,
    r#"if(cli){try{const j=JSON.parse(fs.readFileSync(cli,"utf8"));out.cli_version=j.version;"#,
    r#"out.cli_entry=bin(cli,j);}catch(e){}}"#,
    // Browser bundles. `ffmpeg-*` lives in the same directory and is filtered
    // out: it is not something that can be driven, and counting it would let
    // the pane claim a browser is present when none is.
    r#"try{const bd=process.env.PLAYWRIGHT_BROWSERS_PATH||(home?path.join(home,".cache","ms-playwright"):null);"#,
    r#"if(bd)out.browsers=fs.readdirSync(bd).filter((n)=>/^(chromium|firefox|webkit)/.test(n)).sort();}catch(e){}"#,
    // What this Playwright would *actually launch*, and whether it is there.
    //
    // A cache listing is not the same question. Two Playwright copies in one
    // container — the npx `@playwright/mcp` one and a `/workspace` one — pin
    // different browser revisions, and each installs its own. So the cache can
    // hold `chromium-1237` while the resolved build wants `chromium-1234` and
    // every `chromium.launch()` dies with "Executable doesn't exist", *while
    // the pane reports a browser installed*. Asking the build itself sidesteps
    // revision arithmetic entirely: this is the path a launch would use.
    r#"const exe=(dir)=>{try{const bt=require(dir).chromium;"#,
    r#"const ep=bt&&bt.executablePath?bt.executablePath():null;"#,
    r#"return ep?[ep,fs.existsSync(ep)]:null;}catch(e){return null;}};"#,
    r#"if(core){const r=exe(path.dirname(core));"#,
    r#"if(r){out.chromium_executable=r[0];out.chromium_executable_exists=r[1];}}"#,
    // And separately: what a *script* gets. `require("playwright")` is what
    // every Playwright example writes, and it resolves the wrapper — which
    // carries its own nested `playwright-core` whenever npm could not settle on
    // one version. That copy can want a different browser revision than the one
    // the viewer's copy installed, so it is asked its own question.
    r#"try{const w=res("playwright/package.json");"#,
    r#"if(w){const j=JSON.parse(fs.readFileSync(w,"utf8"));out.script_playwright_version=j.version;"#,
    r#"const wc=at("playwright-core/package.json",path.dirname(w));"#,
    r#"const r=exe(path.dirname(wc||w));"#,
    r#"if(r){out.script_chromium_executable=r[0];out.script_chromium_executable_exists=r[1];}}}catch(e){}"#,
    // The Chrome *channel* is an apt package, not a Playwright download, so it
    // is looked for where apt puts it.
    r#"try{for(const p of ["/usr/bin/google-chrome-stable","/usr/bin/google-chrome","/opt/google/chrome/chrome"]){"#,
    r#"if(fs.existsSync(p)){out.chrome_channel=p;break;}}}catch(e){}"#,
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
    fn a_missing_playwright_names_both_packages_and_where_we_looked() {
        let d = parse_probe_output(&payload(
            r#"{"node_version":"22.11.0","searched":["/workspace","/usr/lib/node_modules","/home/claude/.npm/_npx/a1/node_modules"]}"#,
        ))
        .unwrap();
        assert!(!d.is_usable());
        let msg = d.blocker().unwrap();
        // The two packages that actually have to be there, by name.
        assert!(msg.contains("`playwright`"), "{}", msg);
        assert!(msg.contains("`@playwright/cli`"), "{}", msg);
        assert!(msg.contains("browser.bind"), "{}", msg);
        // Every root consulted, including the npx cache, so the claim is checkable.
        assert!(msg.contains("/usr/lib/node_modules"), "{}", msg);
        assert!(msg.contains("/home/claude/.npm/_npx/a1/node_modules"), "{}", msg);
    }

    #[test]
    fn no_message_offers_playwright_mcp_as_a_way_through_setup() {
        // It bundles a playwright-core new enough to bind, but never ships the
        // viewer — so proposing it as an install route is a dead end, which is
        // exactly what a user hit. It may only be named for what it does do.
        for json in [
            r#"{"node_version":"22.11.0","searched":["/workspace"]}"#,
            r#"{"node_version":"22.11.0","playwright_version":"1.44.0","has_bind":false}"#,
            r#"{"node_version":"22.11.0","playwright_version":"1.62.1","has_bind":true}"#,
        ] {
            let msg = parse_probe_output(&payload(json)).unwrap().blocker().unwrap();
            let offers_install = msg.contains("install `@playwright/mcp`")
                || msg.contains("or use `@playwright/mcp`")
                || msg.contains("npm i -D @playwright/mcp")
                || msg.contains("npm i -g @playwright/mcp");
            assert!(!offers_install, "{}", msg);
            // And every message points at the one action that does work.
            assert!(msg.contains("Set up Playwright"), "{}", msg);
        }
    }

    #[test]
    fn a_playwright_without_bind_asks_for_an_upgrade() {
        let d = parse_probe_output(&payload(
            r#"{"node_version":"22.11.0","playwright_version":"1.44.0","playwright_path":"/workspace/node_modules/playwright/package.json","has_bind":false,"cli_entry":"/x/cli.js"}"#,
        ))
        .unwrap();
        let msg = d.blocker().unwrap();
        assert!(msg.contains("1.44.0"), "{}", msg);
        assert!(msg.contains("/workspace/node_modules/playwright"), "{}", msg);
        assert!(msg.contains("Set up Playwright"), "{}", msg);
    }

    #[test]
    fn an_npx_cached_playwright_counts_as_installed() {
        // What `claude mcp add … npx @playwright/mcp@latest` leaves behind: a
        // real playwright-core, in no `node_modules` the old probe looked at.
        // It satisfies bind — and nothing else, because npx never brings the
        // viewer with it.
        let d = parse_probe_output(&payload(
            concat!(
                r#"{"node_version":"22.11.0","playwright_version":"1.62.1","#,
                r#""playwright_path":"/home/claude/.npm/_npx/9f/node_modules/playwright-core/package.json","#,
                r#""playwright_cli":"/home/claude/.npm/_npx/9f/node_modules/playwright-core/cli.js","#,
                r#""has_bind":true,"#,
                r#""searched":["/workspace","/usr/lib/node_modules","/home/claude/.npm/_npx/9f/node_modules"]}"#,
            ),
        ))
        .unwrap();
        assert_eq!(d.playwright_version.as_deref(), Some("1.62.1"));
        assert!(d.has_bind);
        assert_eq!(
            d.playwright_cli.as_deref(),
            Some("/home/claude/.npm/_npx/9f/node_modules/playwright-core/cli.js")
        );
        // Still not usable, and the message says why: the viewer is missing.
        assert!(!d.is_usable());
        let msg = d.blocker().unwrap();
        assert!(msg.contains("@playwright/cli"), "{}", msg);
    }

    #[test]
    fn the_probe_searches_the_npx_cache_as_well_as_the_module_roots() {
        // The roots are built inside the probe, so this is the only place the
        // set can be asserted without a container. Each fragment is load-bearing:
        // dropping any one of them is how an install becomes invisible.
        assert!(PROBE.contains(r#""/workspace""#), "{}", PROBE);
        assert!(PROBE.contains("process.cwd()"), "{}", PROBE);
        assert!(PROBE.contains(r#"path.join(home,"node_modules")"#), "{}", PROBE);
        assert!(PROBE.contains("npm root -g"), "{}", PROBE);
        assert!(PROBE.contains(r#"path.join(cache,"_npx")"#), "{}", PROBE);
        assert!(PROBE.contains("npm_config_cache"), "{}", PROBE);
        // Every one of them, not just the first hit, and all of them reported.
        assert!(PROBE.contains("...npx"), "{}", PROBE);
        assert!(PROBE.contains("out.searched=roots"), "{}", PROBE);
    }

    #[test]
    fn a_partial_tree_still_answers_rather_than_failing() {
        // Playwright resolved, but its manifest unreadable and no viewer: the
        // probe's guards must still produce a parseable payload carrying what
        // it did learn, because that is what the message is built from.
        let d = parse_probe_output(&payload(
            r#"{"node_version":"22.11.0","has_bind":false,"searched":["/workspace"],"browsers":["chromium-1200"]}"#,
        ))
        .unwrap();
        assert_eq!(d.node_version.as_deref(), Some("22.11.0"));
        assert_eq!(d.browsers, vec!["chromium-1200".to_string()]);
        assert!(d.blocker().is_some());
    }

    #[test]
    fn a_playwright_with_no_browser_bundle_is_flagged_without_blocking() {
        let d = parse_probe_output(&payload(
            concat!(
                r#"{"node_version":"22.11.0","playwright_version":"1.62.1","has_bind":true,"#,
                r#""cli_version":"0.1.18","cli_entry":"/g/cli.js","browsers":[]}"#,
            ),
        ))
        .unwrap();
        // Serving the viewer is possible; there is just nothing to drive yet.
        assert!(d.is_usable());
        assert_eq!(d.blocker(), None);
        assert!(d.needs_browser());

        let with_browser = parse_probe_output(&payload(
            concat!(
                r#"{"node_version":"22.11.0","playwright_version":"1.62.1","has_bind":true,"#,
                r#""cli_version":"0.1.18","cli_entry":"/g/cli.js","browsers":["chromium-1200"]}"#,
            ),
        ))
        .unwrap();
        assert!(!with_browser.needs_browser());

        // The Chrome channel counts too — it is an apt package rather than a
        // Playwright download, so it never appears in `browsers`, and
        // `@playwright/mcp` is the caller that asks for it.
        let chrome_only = parse_probe_output(&payload(
            concat!(
                r#"{"node_version":"22.11.0","playwright_version":"1.62.1","has_bind":true,"#,
                r#""cli_version":"0.1.18","cli_entry":"/g/cli.js","browsers":[],"#,
                r#""chrome_channel":"/usr/bin/google-chrome-stable"}"#,
            ),
        ))
        .unwrap();
        assert!(!chrome_only.needs_browser());
        assert_eq!(
            chrome_only.chrome_channel.as_deref(),
            Some("/usr/bin/google-chrome-stable")
        );
    }

    #[test]
    fn the_probe_looks_for_the_chrome_channel_where_apt_puts_it() {
        assert!(PROBE.contains("google-chrome-stable"), "{}", PROBE);
        assert!(PROBE.contains("/opt/google/chrome/chrome"), "{}", PROBE);
    }

    #[test]
    fn the_probe_asks_playwright_what_it_would_launch() {
        // Not derived from the cache listing — asked of the build, because the
        // cache can hold a browser this build will never launch.
        assert!(PROBE.contains("executablePath"), "{}", PROBE);
        assert!(PROBE.contains("out.chromium_executable_exists"), "{}", PROBE);
    }

    /// A container carrying browsers from a *different* Playwright copy.
    fn skewed() -> PlaywrightDetection {
        parse_probe_output(&payload(concat!(
            r#"{"node_version":"22.11.0","playwright_version":"1.62.1","has_bind":true,"#,
            r#""cli_version":"0.1.18","cli_entry":"/g/cli.js","browsers":["chromium-1237"],"#,
            r#""chromium_executable":"/home/claude/.cache/ms-playwright/chromium-1234/chrome-linux64/chrome","#,
            r#""chromium_executable_exists":false}"#,
        )))
        .unwrap()
    }

    #[test]
    fn a_browser_cache_full_of_the_wrong_revision_counts_as_no_browser() {
        let d = skewed();
        // The viewer still serves — it is the browser that cannot start.
        assert!(d.is_usable());
        assert_eq!(d.blocker(), None);
        assert!(d.revision_skew());
        assert!(d.needs_browser(), "a browser that cannot launch is not a browser");
    }

    #[test]
    fn the_skew_message_names_both_revisions_and_the_way_out() {
        let msg = skewed().skew_message().unwrap();
        assert!(msg.contains("chromium-1237"), "{}", msg); // what is there
        assert!(msg.contains("chromium-1234"), "{}", msg); // what it wants
        assert!(msg.contains("Install Chromium"), "{}", msg); // what fixes it
    }

    #[test]
    fn the_chrome_channel_covers_a_skewed_cache() {
        // The channel is an apt binary at a fixed path, so a revision mismatch
        // cannot affect it: there is still something to drive.
        let mut d = skewed();
        d.chrome_channel = Some("/usr/bin/google-chrome-stable".to_string());
        assert!(!d.needs_browser());
    }

    #[test]
    fn a_probe_that_could_not_answer_is_not_reported_as_skew() {
        // Older container, or a Playwright too broken to `require`: unset is
        // "unknown", and unknown must never render as "your browsers are wrong".
        let d = parse_probe_output(&payload(concat!(
            r#"{"node_version":"22.11.0","playwright_version":"1.62.1","has_bind":true,"#,
            r#""cli_version":"0.1.18","cli_entry":"/g/cli.js","browsers":["chromium-1237"]}"#,
        )))
        .unwrap();
        assert!(!d.revision_skew());
        assert!(!d.needs_browser());
        assert_eq!(d.skew_message(), None);
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
    fn the_probe_reads_bind_from_the_nested_core_of_a_wrapper_install() {
        // `npm i -g playwright` leaves `playwright-core` under
        // `playwright/node_modules`, and the wrapper ships no
        // `types/types.d.ts` — so without this hop a current build reports
        // `has_bind: false`. Verified against a real global install.
        assert!(
            PROBE.contains(r#"at("playwright-core/package.json",path.dirname(pw))"#),
            "{}",
            PROBE
        );
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
