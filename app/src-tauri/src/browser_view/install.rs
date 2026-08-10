//! One-action setup for the browser view.
//!
//! The pane used to answer "Playwright isn't installed" with a sentence of npm
//! commands and leave the user to it. That went badly, and every part of how it
//! went badly is a constraint on this module:
//!
//!  * `claude mcp add … npx @playwright/mcp@latest` looks like installing
//!    Playwright and is not — see [`super::detect`] — and it can never satisfy
//!    this pane on its own, because the viewer lives in `@playwright/cli`.
//!  * `sudo npm i -g playwright` is what people try next. It works, but the
//!    image leaves npm's prefix at `/usr`, so the unprivileged form fails with
//!    `EACCES` first.
//!  * `playwright install chromium` downloads a browser that then cannot start,
//!    because the base image ships **none** of Chromium's shared libraries
//!    (`libnss3`, `libgbm1`, `libatk*`, `libasound2`, `libcups2`, …). The
//!    download succeeds, the launch fails, and the error reads like a Playwright
//!    bug.
//!  * Getting from there to a working pane took a long tail of further commands.
//!
//! ## Where the packages go, and why it is `/workspace`
//!
//! `playwright` + `@playwright/cli`, installed **locally into `/workspace`** as
//! `claude`, with `--no-save`.
//!
//! `/workspace` is *not* the user's repository. Project directories are
//! bind-mounted one level down, at `/workspace/{mount_name}` (see
//! `container_config`), so `/workspace` itself is ordinary container storage.
//! That single fact settles the choice:
//!
//!  * **Nothing of the user's is mutated.** `/workspace/node_modules` is not
//!    inside any bind mount, so no host file, no `package.json` and no lockfile
//!    of theirs is touched. `--no-save` is belt and braces for the case where
//!    someone has put a `package.json` at `/workspace` themselves — verified
//!    that an install into a directory without one writes `node_modules` and
//!    nothing else.
//!  * **No sudo.** `/usr/lib/node_modules` is root-owned; `/workspace` is the
//!    `claude` user's own working directory. A setup action that needs no
//!    privilege escalation is a setup action with one fewer way to fail.
//!  * **Node can actually find it.** A global install is *not* on the module
//!    resolution path — `require('playwright')` from a script in
//!    `/workspace/my-project` does not see `/usr/lib/node_modules`, but it does
//!    walk up to `/workspace/node_modules`. Since the whole point is for Claude
//!    to drive a browser from a script in the project, this is the difference
//!    between setup that works and setup that merely reports as complete.
//!  * **It hoists.** A local install puts `playwright-core` at the top of the
//!    tree where the probe finds it directly; `npm i -g` leaves it nested (see
//!    the note in [`super::detect`]).
//!  * **It persists.** `/workspace` outside the bind mounts rides the project's
//!    snapshot image across container recreation, and migration copies the
//!    non-bind-mounted parts of `/workspace` forward.
//!
//! Browsers are the exception and are downloaded as `claude` into
//! `~/.cache/ms-playwright`, inside the home volume — so they survive
//! recreation *and* base-image migration, and are lost only on a project Reset.
//! That is worth saying in the UI: it is the difference between a
//! several-hundred-megabyte download once and one every time.

use std::collections::VecDeque;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use tauri::AppHandle;

use crate::commands::project_commands::emit_progress;
use crate::docker::exec::{
    create_attached_exec_as, exec_oneshot_as, wait_for_exec_exit, AttachedExec,
};

use super::detect::{self, PlaywrightDetection};

/// The two packages the pane genuinely needs, pinned to `@latest` because
/// `browser.bind()` is recent and the viewer tracks it.
///
/// This is the *minimum* set. A user who followed the old guidance ended up
/// with a global install as well as these; only these are required. Note what
/// is not here: `@playwright/mcp` is Claude's MCP configuration to make, not
/// this pane's, and it contributes nothing to serving a viewer.
pub const PACKAGES: [&str; 2] = ["playwright@latest", "@playwright/cli@latest"];

/// Where the packages are installed. Container storage, not a bind mount — see
/// the module docs.
pub const INSTALL_DIR: &str = "/workspace";

/// npm reaching the registry and unpacking two packages. Minutes, not hours.
const NPM_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// `apt-get update` plus a dozen library packages, or Google's apt repository.
const DEPS_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// The browser download itself, on a bad connection.
const BROWSER_TIMEOUT: Duration = Duration::from_secs(45 * 60);
/// Starting a headless browser, and one page load.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(120);

/// Lines of command output kept for the result. Enough to carry npm's actual
/// error, bounded so a chatty download can't grow without limit.
const LOG_LINES: usize = 200;

/// Longest run of bytes without a line break still treated as one progress
/// line. npm's progress output is `\r`-driven and can go a long way.
const MAX_PARTIAL: usize = 4 * 1024;

/// Marks the verdict in the launch check's output, for the same reason
/// [`super::detect`] marks its payload: the stream also carries whatever
/// Chromium felt like writing to stderr.
const LAUNCH_MARKER: &str = "__TRIPLE_C_BROWSER_LAUNCH__";

/// URL the launch check navigates to once a browser is up. The registry is the
/// one host we know the container just reached, during the npm step — so a
/// failure here is informative rather than ambient.
const REACHABILITY_URL: &str = "https://registry.npmjs.org/-/ping";

/// Which browser to install. Both are legitimate; they serve different callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserTarget {
    /// Playwright's own build. What `chromium.launch()` uses with no `channel`,
    /// i.e. the default for scripts the user or Claude writes.
    Chromium,
    /// Google Chrome from Google's apt repository. **`@playwright/mcp` asks for
    /// the `chrome` channel specifically**, so anyone driving the browser
    /// through the MCP plugin needs this one rather than (or as well as) the
    /// bundled build.
    Chrome,
}

impl BrowserTarget {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "chromium" => Ok(Self::Chromium),
            "chrome" => Ok(Self::Chrome),
            other => Err(format!(
                "Unknown browser '{}'. This pane installs 'chromium' (Playwright's own build) \
                 or 'chrome' (the Google Chrome channel that @playwright/mcp asks for).",
                other
            )),
        }
    }

    /// The name Playwright's CLI knows it by.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Chromium => "chromium",
            Self::Chrome => "chrome",
        }
    }

    /// Shown *before* the click. Size first, because the honest failure mode
    /// here is a user who did not know they were starting a large download.
    pub fn download_note(self) -> &'static str {
        match self {
            Self::Chromium => {
                "Playwright's Chromium build plus the system libraries it needs — several \
                 hundred MB in total, a few minutes on a normal connection"
            }
            Self::Chrome => {
                "Google Chrome from Google's apt repository, with its dependencies — roughly \
                 150 MB"
            }
        }
    }

    /// Who needs it, in the user's terms.
    pub fn needed_for(self) -> &'static str {
        match self {
            Self::Chromium => "Playwright scripts that call `chromium.launch()` with no channel",
            Self::Chrome => "`@playwright/mcp`, which asks for the `chrome` channel",
        }
    }

    /// The `channel` a launch check must pass. `None` means the bundled build.
    fn channel(self) -> Option<&'static str> {
        match self {
            Self::Chromium => None,
            Self::Chrome => Some("chrome"),
        }
    }
}

/// What a setup step did, handed back to the pane so it can update itself
/// without the user reopening the tab.
#[derive(Debug, Clone, Serialize)]
pub struct BrowserSetupOutcome {
    /// A fresh probe, run after the step. This is what makes the pane
    /// self-updating.
    pub detection: PlaywrightDetection,
    /// Tail of the actual command output — always populated, success or not.
    pub log: String,
    /// Verdict of a real headless launch. `None` when the step didn't try one
    /// (the npm step doesn't), `Some(false)` when a browser is installed and
    /// still would not start.
    pub browser_launched: Option<bool>,
    /// A step that failed, or succeeded suspiciously, without failing the whole
    /// action — the user is told rather than left to find out at launch time.
    pub warning: Option<String>,
}

/// Install `playwright` and `@playwright/cli` into `/workspace`. Deliberately
/// does *not* fetch browsers: that is the second step, and its size is stated
/// before it is offered.
pub async fn install_packages(
    app: &AppHandle,
    project_id: &str,
    container_id: &str,
) -> Result<BrowserSetupOutcome, String> {
    emit_progress(
        app,
        project_id,
        &format!(
            "Installing playwright and @playwright/cli into {}/node_modules…",
            INSTALL_DIR
        ),
    );

    // `env VAR=… cmd` rather than an exec env: it keeps the one exec path in
    // `docker/exec.rs` untouched, and `env` is a real binary so no shell is
    // involved. The guard matters because these are `@latest`: current
    // Playwright has no postinstall (verified — `playwright@1.62.1` declares no
    // `scripts` at all), but if a future release brings the browser download
    // back, this step must stay small and the download must stay the step the
    // user explicitly asked for.
    let mut cmd = vec![
        "env".to_string(),
        "PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1".to_string(),
        "npm".to_string(),
        "install".to_string(),
        // Leaves any package.json and lockfile at /workspace untouched.
        "--no-save".to_string(),
        "--no-fund".to_string(),
        "--no-audit".to_string(),
    ];
    cmd.extend(PACKAGES.iter().map(|p| p.to_string()));

    let step = run_step(
        app,
        project_id,
        container_id,
        "claude",
        INSTALL_DIR,
        cmd,
        NPM_TIMEOUT,
    )
    .await?;
    if step.exit_code != 0 {
        return Err(format!(
            "npm couldn't install Playwright in this container (exit {}).\n\nnpm said:\n{}",
            step.exit_code,
            step.log_or("it produced no output at all")
        ));
    }

    emit_progress(app, project_id, "Re-checking what the container has…");
    let detection = detect::detect(container_id).await?;

    // A probe that still finds something missing after a successful npm run is
    // the interesting case, so it is surfaced rather than swallowed. And a
    // container with the packages but no browser is *not* finished setup —
    // saying so here is what stops someone walking away from a pane that will
    // never show them anything.
    let mut warning = detection.blocker();
    if detection.needs_browser() {
        warning = merge(
            warning,
            "Playwright is installed, but this container has no browser to drive yet. Install \
             one below — that is the large download, and it is a separate step on purpose."
                .to_string(),
        );
    }

    Ok(BrowserSetupOutcome {
        warning,
        detection,
        log: step.log,
        browser_launched: None,
    })
}

/// Install a browser: its system libraries first, then the browser, then prove
/// one actually starts.
///
/// The order is the whole point. `playwright install chromium` on this image
/// downloads a browser that cannot launch, because the libraries it links
/// against are absent — which is why installing Chrome through apt looked like
/// the fix: apt pulls those libraries in as dependencies.
pub async fn install_browser(
    app: &AppHandle,
    project_id: &str,
    container_id: &str,
    target: BrowserTarget,
) -> Result<BrowserSetupOutcome, String> {
    let before = detect::detect(container_id).await?;
    let Some(cli) = before.playwright_cli.clone() else {
        return Err(
            "Playwright isn't installed in this container yet, so there is nothing to install a \
             browser for. Run “Set up Playwright” first."
                .to_string(),
        );
    };

    let mut log = String::new();
    let mut warning: Option<String> = None;

    // Step 1 — system libraries. Slow, previously invisible, and the actual
    // cause of the "Chromium downloads and then dies" reports, so it gets its
    // own progress line rather than being folded into the download.
    //
    // This is what `playwright install --with-deps` does internally. Running
    // `install-deps` directly *as root* is the same apt work without depending
    // on Playwright's own privilege escalation: read from the shipped source, it
    // shells out to `sudo -- sh -c "apt-get update && apt-get install …"` when
    // it is not root, which would work here (`claude` has passwordless sudo) but
    // puts an extra failure mode between the user and the answer.
    //
    // For the Chrome channel apt installs `google-chrome-stable`, whose own
    // dependencies cover the same libraries — but running `install-deps` first
    // costs little and makes the two paths behave identically.
    emit_progress(
        app,
        project_id,
        "Step 1/3 — installing browser system libraries with apt (needs root; a minute or two)…",
    );
    let deps = run_step(
        app,
        project_id,
        container_id,
        "root",
        "/tmp",
        vec![
            "node".to_string(),
            cli.clone(),
            "install-deps".to_string(),
            target.cli_name().to_string(),
        ],
        DEPS_TIMEOUT,
    )
    .await?;
    log.push_str(&deps.log);
    if deps.exit_code != 0 {
        // Not fatal on its own — the libraries may already be present — but it
        // must never pass silently, because the failure it causes surfaces much
        // later and looks like something else.
        warning = Some(format!(
            "Installing the browser's system libraries failed (exit {}). The browser may install \
             and then refuse to start. apt said:\n{}",
            deps.exit_code,
            deps.log_or("nothing")
        ));
    }

    // Step 2 — the download the user was warned about. Chromium is fetched as
    // `claude` so the bundle lands in the home volume's `~/.cache/ms-playwright`
    // where the viewer looks for it; the Chrome channel is an apt install and
    // has to be root.
    emit_progress(
        app,
        project_id,
        &format!(
            "Step 2/3 — installing {}, needed for {} — {}…",
            target.cli_name(),
            target.needed_for(),
            target.download_note()
        ),
    );
    let (user, workdir) = match target {
        BrowserTarget::Chromium => ("claude", INSTALL_DIR),
        BrowserTarget::Chrome => ("root", "/tmp"),
    };
    let dl = run_step(
        app,
        project_id,
        container_id,
        user,
        workdir,
        vec![
            "node".to_string(),
            cli,
            "install".to_string(),
            target.cli_name().to_string(),
        ],
        BROWSER_TIMEOUT,
    )
    .await?;
    push_section(&mut log, &dl.log);
    if dl.exit_code != 0 {
        return Err(format!(
            "{} didn't install (exit {}).\n\nPlaywright said:\n{}",
            target.cli_name(),
            dl.exit_code,
            dl.log_or("nothing")
        ));
    }

    // Step 3 — "installed" is not "works". The absence of this check is what
    // turned a missing library into a pile of confusing errors.
    emit_progress(
        app,
        project_id,
        &format!("Step 3/3 — checking that {} actually launches…", target.cli_name()),
    );
    let verdict = verify_launch(container_id, &before, target).await;
    push_section(&mut log, &verdict.detail);
    if !verdict.ok {
        warning = merge(
            warning,
            format!(
                "{} is installed but would not start: {}",
                target.cli_name(),
                verdict.detail.trim()
            ),
        );
    }
    if let Some(cert) = verdict.cert_error {
        // Distinct on purpose. A TLS-intercepting proxy is a container-wide
        // trust-store gap — it breaks npm, git, curl and Claude Code the same
        // way — and telling someone behind one that Playwright is broken sends
        // them to fix the wrong thing. This pane does not install CAs.
        warning = merge(
            warning,
            format!(
                "The browser starts, but HTTPS pages fail certificate validation ({}). That is \
                 this container not trusting your network's certificate authority — a TLS-\
                 intercepting proxy — and it affects everything in the container, not just the \
                 browser. Installing the CA into the container's trust store is the fix; this \
                 pane doesn't do that.",
                cert.trim()
            ),
        );
    }

    emit_progress(app, project_id, "Re-checking what the container has…");
    let detection = detect::detect(container_id).await?;

    Ok(BrowserSetupOutcome {
        detection,
        log: log.trim().to_string(),
        browser_launched: Some(verdict.ok),
        warning,
    })
}

/// The result of actually starting a browser.
#[derive(Debug, Clone)]
struct LaunchVerdict {
    ok: bool,
    detail: String,
    /// Set when a page load failed specifically on certificate trust.
    cert_error: Option<String>,
}

/// Launch the browser headless, load one page, and close it. Seconds, and it is
/// the only thing that distinguishes "downloaded" from "usable".
async fn verify_launch(
    container_id: &str,
    detection: &PlaywrightDetection,
    target: BrowserTarget,
) -> LaunchVerdict {
    // The module directory of whatever Playwright the probe resolved, passed in
    // the environment rather than interpolated into the script, so no path can
    // ever be read as JavaScript.
    let Some(manifest) = detection.playwright_path.as_deref() else {
        return LaunchVerdict {
            ok: false,
            detail: "Playwright could not be located to test with.".to_string(),
            cert_error: None,
        };
    };
    let dir = manifest.trim_end_matches("package.json").trim_end_matches('/');

    let run = exec_oneshot_as(
        container_id,
        "claude",
        vec![
            "node".to_string(),
            "-e".to_string(),
            LAUNCH_PROBE.to_string(),
        ],
        vec![
            format!("TRIPLE_C_PW_DIR={}", dir),
            format!("TRIPLE_C_PW_CHANNEL={}", target.channel().unwrap_or("")),
            format!("TRIPLE_C_PW_URL={}", REACHABILITY_URL),
        ],
    );
    match tokio::time::timeout(VERIFY_TIMEOUT, run).await {
        Ok(Ok((output, _))) => parse_launch_output(&output),
        Ok(Err(e)) => LaunchVerdict {
            ok: false,
            detail: e,
            cert_error: None,
        },
        Err(_) => LaunchVerdict {
            ok: false,
            detail: "the launch check didn't finish in time — treat the browser as unproven"
                .to_string(),
            cert_error: None,
        },
    }
}

/// Pull the verdict out of the launch check's combined output.
fn parse_launch_output(output: &str) -> LaunchVerdict {
    let Some(idx) = output.find(LAUNCH_MARKER) else {
        let trimmed = output.trim();
        return LaunchVerdict {
            ok: false,
            detail: if trimmed.is_empty() {
                "the launch check produced no output".to_string()
            } else {
                trimmed.lines().next_back().unwrap_or(trimmed).trim().to_string()
            },
            cert_error: None,
        };
    };
    let line = output[idx + LAUNCH_MARKER.len()..]
        .lines()
        .next()
        .unwrap_or("")
        .trim();
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return LaunchVerdict {
                ok: false,
                detail: format!("unreadable launch check result: {}", e),
                cert_error: None,
            }
        }
    };

    let ok = value.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    let detail = value
        .get("detail")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();
    let nav = value.get("nav");
    let cert_error = nav
        .filter(|n| n.get("cert").and_then(|c| c.as_bool()).unwrap_or(false))
        .and_then(|n| n.get("detail").and_then(|d| d.as_str()))
        .map(|s| s.to_string());

    let detail = if !ok {
        detail
    } else {
        match nav.and_then(|n| n.get("ok").and_then(|o| o.as_bool())) {
            // A launch that works and a page that loads: setup is genuinely done.
            Some(true) => format!("Browser launched ({}) and loaded a page.", detail),
            // Launch fine, page not. Cert failures are reported separately; any
            // other cause is likely just an offline container, so it is stated
            // without being turned into an alarm.
            Some(false) => format!(
                "Browser launched ({}), but couldn't load a test page: {}",
                detail,
                nav.and_then(|n| n.get("detail").and_then(|d| d.as_str()))
                    .unwrap_or("no detail")
            ),
            _ => format!("Browser launched ({}).", detail),
        }
    };

    LaunchVerdict {
        ok,
        detail,
        cert_error,
    }
}

/// The launch check. One `argv` element, no newlines, same contract as the
/// detection probe.
///
/// Playwright leaves the Chromium sandbox disabled by default, which is what
/// makes this work in a container at all. The timeout exists so a browser that
/// hangs on a missing library still returns a verdict rather than sitting there
/// until the exec is torn down. The navigation is best-effort and never decides
/// `ok` — it exists to tell a TLS-intercepted network apart from a broken
/// install.
const LAUNCH_PROBE: &str = concat!(
    r#"const d=process.env.TRIPLE_C_PW_DIR,ch=process.env.TRIPLE_C_PW_CHANNEL||undefined,u=process.env.TRIPLE_C_PW_URL;"#,
    r#"let done=false;const say=(ok,detail,nav)=>{if(done)return;done=true;"#,
    r#"process.stdout.write("\n__TRIPLE_C_BROWSER_LAUNCH__"+JSON.stringify({ok,detail,nav:nav||null})+"\n");};"#,
    r#"const one=(e)=>String((e&&e.message)||e).split("\n").slice(0,8).join(" | ");"#,
    r#"const t=setTimeout(()=>{say(false,"the browser did not finish starting within 90s");process.exit(0);},90000);"#,
    r#"(async()=>{let b=null;try{const {chromium}=require(d);b=await chromium.launch(ch?{channel:ch}:{});"#,
    r#"let v="";try{v=b.version();}catch(e){}"#,
    r#"let nav={ok:true,cert:false,detail:""};"#,
    r#"try{const p=await b.newPage();await p.goto(u,{timeout:20000});}"#,
    // A certificate failure is classified here, next to the message, because
    // Chromium's wording is the only place the distinction exists.
    r#"catch(e){const m=one(e);nav={ok:false,cert:/ERR_CERT|CERT_AUTHORITY|ERR_SSL|SSL_ERROR|self.signed/i.test(m),detail:m};}"#,
    r#"await b.close();clearTimeout(t);say(true,v,nav);}"#,
    r#"catch(e){clearTimeout(t);try{if(b)await b.close();}catch(e2){}say(false,one(e));}"#,
    r#"process.exit(0);})();"#,
);

/// One command's result: its exit code and the tail of what it printed.
struct StepResult {
    exit_code: i64,
    log: String,
}

impl StepResult {
    fn log_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.log.trim().is_empty() {
            fallback
        } else {
            &self.log
        }
    }
}

/// Run one command in the container, streaming every line it prints to the pane
/// as `container-progress` and keeping the tail for the result.
///
/// Uses `create_attached_exec_as` — the single attached-exec path — rather than
/// `exec_oneshot`, because these commands run for minutes and the point is that
/// the user can watch them.
async fn run_step(
    app: &AppHandle,
    project_id: &str,
    container_id: &str,
    user: &str,
    workdir: &str,
    cmd: Vec<String>,
    limit: Duration,
) -> Result<StepResult, String> {
    let AttachedExec {
        exec_id,
        mut output,
        input,
    } = create_attached_exec_as(container_id, cmd, false, user, workdir).await?;

    // Nothing is ever written to this exec. Closing stdin means anything that
    // would prompt sees EOF and gives up, instead of waiting for a person who
    // isn't there.
    drop(input);

    let mut tail: VecDeque<String> = VecDeque::with_capacity(LOG_LINES);
    let mut partial = String::new();

    let pump = async {
        while let Some(msg) = output.next().await {
            let data = msg.map_err(|e| format!("Lost the container's output: {}", e))?;
            let chunk = String::from_utf8_lossy(&data.into_bytes()).into_owned();
            // npm and Playwright both redraw with `\r`; treating that as a line
            // break is what turns a progress bar into progress.
            for piece in chunk.split_inclusive(|c| c == '\n' || c == '\r') {
                partial.push_str(piece);
                if piece.ends_with('\n') || piece.ends_with('\r') || partial.len() >= MAX_PARTIAL {
                    let line = std::mem::take(&mut partial);
                    record(&mut tail, line.trim(), app, project_id);
                }
            }
        }
        Ok::<(), String>(())
    };

    let outcome = tokio::time::timeout(limit, pump).await;
    if !partial.trim().is_empty() {
        let line = std::mem::take(&mut partial);
        record(&mut tail, line.trim(), app, project_id);
    }
    let log = tail.iter().cloned().collect::<Vec<_>>().join("\n");

    match outcome {
        // The captured tail is deliberately part of the timeout message: a step
        // that ran for 45 minutes and stopped has its reason in its last lines.
        Err(_) => Err(format!(
            "The command didn't finish within {} minutes and was abandoned.\n\nLast output:\n{}",
            limit.as_secs() / 60,
            if log.trim().is_empty() { "none" } else { &log }
        )),
        Ok(Err(e)) => Err(e),
        Ok(Ok(())) => Ok(StepResult {
            exit_code: wait_for_exec_exit(&exec_id).await.unwrap_or(0),
            log,
        }),
    }
}

/// Keep a line for the result and show it in the pane.
fn record(tail: &mut VecDeque<String>, line: &str, app: &AppHandle, project_id: &str) {
    if line.is_empty() {
        return;
    }
    if tail.len() == LOG_LINES {
        tail.pop_front();
    }
    tail.push_back(line.to_string());
    emit_progress(app, project_id, &truncate(line, 160));
}

/// Append a further command's output to the accumulated log.
fn push_section(log: &mut String, section: &str) {
    if section.trim().is_empty() {
        return;
    }
    if !log.is_empty() {
        log.push_str("\n\n");
    }
    log.push_str(section.trim());
}

/// Combine warnings so a second one never silently replaces the first.
fn merge(existing: Option<String>, next: String) -> Option<String> {
    Some(match existing {
        Some(prev) => format!("{}\n\n{}", prev, next),
        None => next,
    })
}

/// Progress is a single line in the UI, so an over-long one is cut here rather
/// than allowed to reflow the layout. Cuts on a char boundary.
fn truncate(line: &str, max: usize) -> String {
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut s: String = line.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_package_set_is_the_minimum_that_satisfies_the_probe() {
        // The viewer package is not optional, and `@playwright/mcp` is not a
        // member: it can bind sessions, it can never serve the UI.
        assert!(PACKAGES.iter().any(|p| p.starts_with("playwright@")));
        assert!(PACKAGES.iter().any(|p| p.starts_with("@playwright/cli@")));
        assert!(!PACKAGES.iter().any(|p| p.contains("@playwright/mcp")));
        assert_eq!(PACKAGES.len(), 2);
    }

    #[test]
    fn packages_are_installed_where_no_sudo_and_no_bind_mount_are_involved() {
        // Bind mounts live at /workspace/<mount_name>; /workspace itself does
        // not belong to the user's repository, and does belong to `claude`.
        assert_eq!(INSTALL_DIR, "/workspace");
    }

    #[test]
    fn both_browser_targets_are_offered_and_named_for_who_needs_them() {
        assert_eq!(BrowserTarget::parse("chromium").unwrap(), BrowserTarget::Chromium);
        assert_eq!(BrowserTarget::parse("chrome").unwrap(), BrowserTarget::Chrome);
        // `@playwright/mcp` asks for the chrome channel specifically, so the UI
        // must be able to say so.
        assert!(BrowserTarget::Chrome.needed_for().contains("@playwright/mcp"));
        assert_eq!(BrowserTarget::Chrome.channel(), Some("chrome"));
        assert_eq!(BrowserTarget::Chromium.channel(), None);
        // And a size, before the click, for both.
        for t in [BrowserTarget::Chromium, BrowserTarget::Chrome] {
            assert!(t.download_note().to_lowercase().contains("mb"), "{:?}", t);
        }
    }

    #[test]
    fn an_unknown_browser_is_refused_with_the_two_that_work() {
        let err = BrowserTarget::parse("firefox").unwrap_err();
        assert!(err.contains("chromium"), "{}", err);
        assert!(err.contains("chrome"), "{}", err);
    }

    #[test]
    fn a_successful_launch_and_page_load_is_reported_as_working() {
        let v = parse_launch_output(concat!(
            "some chromium noise\n__TRIPLE_C_BROWSER_LAUNCH__",
            r#"{"ok":true,"detail":"140.0.1","nav":{"ok":true,"cert":false,"detail":""}}"#,
            "\n",
        ));
        assert!(v.ok);
        assert!(v.cert_error.is_none());
        assert!(v.detail.contains("140.0.1"), "{}", v.detail);
        assert!(v.detail.contains("loaded a page"), "{}", v.detail);
    }

    #[test]
    fn a_missing_shared_library_is_reported_verbatim() {
        // The exact failure the base image produces. It must reach the user as
        // itself, not as "installation failed".
        let v = parse_launch_output(concat!(
            "__TRIPLE_C_BROWSER_LAUNCH__",
            r#"{"ok":false,"detail":"Host system is missing dependencies: libnss3.so"}"#,
            "\n",
        ));
        assert!(!v.ok);
        assert!(v.detail.contains("libnss3.so"), "{}", v.detail);
    }

    #[test]
    fn a_tls_intercepting_proxy_is_distinguished_from_a_broken_install() {
        // The browser is fine; the container doesn't trust the network's CA.
        // Reporting this as a launch failure sends the user to fix Playwright.
        let v = parse_launch_output(concat!(
            "__TRIPLE_C_BROWSER_LAUNCH__",
            r#"{"ok":true,"detail":"140.0.1","nav":{"ok":false,"cert":true,"#,
            r#""detail":"page.goto: net::ERR_CERT_AUTHORITY_INVALID at https://registry.npmjs.org/"}}"#,
            "\n",
        ));
        assert!(v.ok, "the browser did launch");
        assert_eq!(
            v.cert_error.as_deref().map(|s| s.contains("ERR_CERT_AUTHORITY_INVALID")),
            Some(true)
        );
    }

    #[test]
    fn an_offline_container_is_not_reported_as_a_certificate_problem() {
        let v = parse_launch_output(concat!(
            "__TRIPLE_C_BROWSER_LAUNCH__",
            r#"{"ok":true,"detail":"140.0.1","nav":{"ok":false,"cert":false,"#,
            r#""detail":"net::ERR_NAME_NOT_RESOLVED"}}"#,
            "\n",
        ));
        assert!(v.ok);
        assert!(v.cert_error.is_none());
        assert!(v.detail.contains("ERR_NAME_NOT_RESOLVED"), "{}", v.detail);
    }

    #[test]
    fn an_unmarked_stream_surfaces_the_containers_own_error() {
        let v = parse_launch_output("node: command not found\n");
        assert!(!v.ok);
        assert!(v.detail.contains("command not found"), "{}", v.detail);
    }

    #[test]
    fn an_empty_stream_is_explained_rather_than_parsed() {
        let v = parse_launch_output("  \n");
        assert!(!v.ok);
        assert!(v.detail.contains("no output"), "{}", v.detail);
    }

    #[test]
    fn the_launch_probe_is_a_single_argv_element() {
        assert!(!LAUNCH_PROBE.contains('\n'));
        assert!(LAUNCH_PROBE.contains(LAUNCH_MARKER));
        // Paths and channels are read from the environment, never interpolated.
        assert!(LAUNCH_PROBE.contains("process.env.TRIPLE_C_PW_DIR"));
        assert!(LAUNCH_PROBE.contains("process.env.TRIPLE_C_PW_CHANNEL"));
        assert!(LAUNCH_PROBE.contains("ERR_CERT"));
    }

    #[test]
    fn warnings_accumulate_rather_than_overwrite() {
        let w = merge(Some("first".to_string()), "second".to_string()).unwrap();
        assert!(w.contains("first") && w.contains("second"), "{}", w);
        assert_eq!(merge(None, "only".to_string()).as_deref(), Some("only"));
    }

    #[test]
    fn long_progress_lines_are_cut_on_a_char_boundary() {
        let line = "é".repeat(400);
        let cut = truncate(&line, 160);
        assert_eq!(cut.chars().count(), 160);
        assert!(cut.ends_with('…'));
        assert_eq!(truncate("short", 160), "short");
    }
}
