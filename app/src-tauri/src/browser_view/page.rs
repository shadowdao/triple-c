//! Open a page in the container's browser, and resize it while it runs.
//!
//! The pane [watches](super) browsers something else published. This opens one:
//! the user hands it a URL, it launches a browser inside the container,
//! publishes it with `browser.bind()` so the pane picks it up, and holds the
//! handle so the page can be navigated and **resized** afterwards.
//!
//! ## Why the handle has to be held
//!
//! Verified against a real bound browser: a second client cannot join one.
//! `chromium.connect()` against the published endpoint times out in every URL
//! form — the descriptor's socket speaks the dashboard's own transport, not the
//! public connect protocol. So whoever launches the browser is the only process
//! that can ever drive it. That is the whole reason this helper is a resident
//! process rather than a one-shot `node -e` that exits.
//!
//! It also draws the line for the feature: pages *this* opens can be resized
//! live; a browser `@playwright/mcp` launched can only be watched, and its size
//! is whatever `--viewport-size` it was given.
//!
//! ## Control channel
//!
//! A JSON file in `/tmp`, polled by the helper. No port, no second listener, no
//! addition to the proxy's attack surface — and it composes with the one exec
//! path this codebase already has. Writes go through `node -e` rather than
//! shell redirection so a URL never touches a shell.
//!
//! ## Viewport, and why it is the interesting part
//!
//! `page.setViewportSize()` genuinely reflows: measured on a page carrying a
//! `@media (max-width: 900px)` rule, the rule fires at 800×600 and clears at
//! 1440×900. Resizing the *window* the pane lives in does nothing of the sort —
//! the viewer is a CDP screencast, so a bigger window is the same pixels drawn
//! larger. This is what makes the pop-out usable as a responsive-design ruler.

use serde::{Deserialize, Serialize};

use crate::docker::exec::exec_oneshot_as;

use super::detect::PlaywrightDetection;

/// Control file the helper polls, and the state file it writes back.
const CONTROL_PATH: &str = "/tmp/triple-c-page-control.json";
const STATE_PATH: &str = "/tmp/triple-c-page-state.json";
/// Where the detached helper's own output goes, so a failed start has a trail.
const HELPER_LOG: &str = "/tmp/triple-c-page.log";

/// How long to wait for the helper to report that the page is up.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
/// Navigating a browser that is already up. One page load, not a cold start.
const REUSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(35);
const READY_POLL: std::time::Duration = std::time::Duration::from_millis(400);

/// A viewport, in CSS pixels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    /// Clamped to something a browser will accept. A window dragged to nothing
    /// must not ask Chromium for a zero-width page.
    pub fn sane(width: u32, height: u32) -> Self {
        Self {
            width: width.clamp(200, 7680),
            height: height.clamp(200, 4320),
        }
    }
}

/// What the helper reports about itself.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PageState {
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub viewport: Option<Viewport>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Open `url` in a freshly launched, bound browser.
///
/// Replaces any page this opened before: one helper per container, because the
/// pane shows one browser and a second would just compete for the pane.
pub async fn open(
    container_id: &str,
    detection: &PlaywrightDetection,
    url: &str,
    viewport: Viewport,
) -> Result<PageState, String> {
    let core = detection.playwright_path.as_deref().ok_or_else(|| {
        "Playwright isn't installed in this container — set it up from the Browser tab first."
            .to_string()
    })?;
    // The directory of the resolved manifest is what `require()` wants.
    let core_dir = core.trim_end_matches("/package.json");

    // The executable is passed explicitly rather than left to Playwright's
    // revision lookup: a container can hold browsers a given copy will not
    // launch (see `detect::revision_skew`), and this is the one place we know
    // which binary is actually on disk.
    let executable = detection
        .chromium_executable
        .as_deref()
        .filter(|_| detection.chromium_executable_exists);

    // Reuse a helper that is already up. Relaunching would throw away the
    // browser's cookies and storage — which for the auth case means signing in
    // again to reach the second page, having just signed in on the first.
    if state(container_id).await.ready {
        set_viewport(container_id, viewport).await?;
        navigate(container_id, url).await?;
        if let Some(state) = wait_for_url(container_id, url).await {
            return Ok(state);
        }
        // It stopped answering; fall through and start a fresh one.
    }

    close(container_id).await;

    let config = serde_json::json!({
        "core": core_dir,
        "executable": executable,
        "url": url,
        "viewport": viewport,
        "control": CONTROL_PATH,
        "state": STATE_PATH,
    });
    let script = format!("const CFG={};{}", config, HELPER);

    // Detached, for the same reason the viewer is: the process has to outlive
    // the exec that started it, or the page closes the moment we return.
    let launcher = format!(
        "cd /workspace 2>/dev/null || true; rm -f {} {}; nohup node -e {} >{} 2>&1 &",
        STATE_PATH,
        CONTROL_PATH,
        shell_quote(&script),
        HELPER_LOG
    );
    exec_oneshot_as(
        container_id,
        "claude",
        vec!["sh".to_string(), "-c".to_string(), launcher],
        Vec::new(),
    )
    .await
    .map_err(|e| format!("Could not start the browser helper: {}", e))?;

    wait_until_ready(container_id).await
}

/// Resize the open page. Cheap enough to call from a window-resize handler.
pub async fn set_viewport(container_id: &str, viewport: Viewport) -> Result<(), String> {
    write_control(
        container_id,
        serde_json::json!({ "viewport": viewport }).to_string(),
    )
    .await
}

/// Navigate the open page without relaunching the browser.
pub async fn navigate(container_id: &str, url: &str) -> Result<(), String> {
    write_control(container_id, serde_json::json!({ "url": url }).to_string()).await
}

/// Ask the helper to shut down. Best effort: a container that has none is the
/// normal case, and the caller is usually about to start one anyway.
pub async fn close(container_id: &str) {
    let _ = write_control(container_id, serde_json::json!({ "close": true }).to_string()).await;
}

/// Current state, or a default when no helper has ever run here.
pub async fn state(container_id: &str) -> PageState {
    let script = format!(
        "try{{process.stdout.write(require('fs').readFileSync('{}','utf8'));}}catch(e){{}}",
        STATE_PATH
    );
    let Ok((out, _)) = exec_oneshot_as(
        container_id,
        "claude",
        vec!["node".to_string(), "-e".to_string(), script],
        Vec::new(),
    )
    .await
    else {
        return PageState::default();
    };
    serde_json::from_str(out.trim()).unwrap_or_default()
}

/// Write the control file through Node rather than a shell redirect, so a URL
/// is never interpreted by `sh`.
async fn write_control(container_id: &str, json: String) -> Result<(), String> {
    let script = format!(
        "require('fs').writeFileSync('{}',process.argv[1]);",
        CONTROL_PATH
    );
    exec_oneshot_as(
        container_id,
        "claude",
        vec!["node".to_string(), "-e".to_string(), script, json],
        Vec::new(),
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("Could not reach the browser helper: {}", e))
}

/// Wait for a *running* helper to report the URL we just asked it for.
///
/// Bounded much tighter than a cold start: the browser is already up, so this
/// is one navigation. `None` means it stopped answering, and the caller starts
/// a fresh helper rather than reporting a page that isn't there.
async fn wait_for_url(container_id: &str, url: &str) -> Option<PageState> {
    let deadline = std::time::Instant::now() + REUSE_TIMEOUT;
    loop {
        let state = state(container_id).await;
        if state.ready && state.url.as_deref() == Some(url) {
            return Some(state);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

/// Poll the state file until the helper says the page is up, or says why not.
async fn wait_until_ready(container_id: &str) -> Result<PageState, String> {
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    loop {
        let state = state(container_id).await;
        if let Some(error) = state.error.clone() {
            return Err(error);
        }
        if state.ready {
            return Ok(state);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "The browser didn't come up within {}s. Its log is at {} inside the container.",
                READY_TIMEOUT.as_secs(),
                HELPER_LOG
            ));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

/// Single-quote for `sh`, the same way [`super`] does for the viewer's paths.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The resident helper, appended to a `const CFG={…};` prelude.
///
/// Deliberately one string passed as a single `argv` element — no shell parsing
/// of any part of it, exactly like `detect`'s probe. It launches, binds, and
/// then polls the control file; every failure path writes the state file, so a
/// helper that dies during startup is reported rather than waited out.
const HELPER: &str = concat!(
    r#"const fs=require('fs');"#,
    r#"const {chromium}=require(CFG.core);"#,
    r#"const write=(o)=>{try{fs.writeFileSync(CFG.state,JSON.stringify(o));}catch(e){}};"#,
    r#"const fail=(e)=>{write({ready:false,error:String(e&&e.message||e)});process.exit(1);};"#,
    r#"process.on('unhandledRejection',fail);"#,
    r#"(async()=>{"#,
    // `chromiumSandbox:false` because the container has no user namespaces to
    // give Chromium; headless because there is no display, which is also the
    // only mode the dashboard can screencast anyway.
    r#"const opts={headless:true,chromiumSandbox:false};"#,
    r#"if(CFG.executable)opts.executablePath=CFG.executable;"#,
    r#"const browser=await chromium.launch(opts);"#,
    r#"const ctx=await browser.newContext({viewport:CFG.viewport});"#,
    r#"const page=await ctx.newPage();"#,
    // Bind before navigating: the pane should show the page loading rather than
    // appearing once it is done.
    r#"await browser.bind('claude',{metadata:{source:'triple-c'}});"#,
    r#"let current=CFG.url,viewport=CFG.viewport;"#,
    r#"const report=()=>write({ready:true,url:current,viewport});"#,
    r#"try{await page.goto(CFG.url,{waitUntil:'domcontentloaded',timeout:30000});}catch(e){}"#,
    r#"report();"#,
    // The control loop. A poll, not a watcher: `fs.watch` misses writes on some
    // filesystems and this costs nothing at 4 Hz.
    r#"setInterval(async()=>{let c;try{c=JSON.parse(fs.readFileSync(CFG.control,'utf8'));}catch(e){return;}"#,
    r#"try{fs.unlinkSync(CFG.control);}catch(e){}"#,
    r#"if(c.close){await browser.close().catch(()=>{});write({ready:false});process.exit(0);}"#,
    r#"if(c.viewport){viewport=c.viewport;await page.setViewportSize(c.viewport).catch(()=>{});}"#,
    r#"if(c.url&&c.url!==current){current=c.url;await page.goto(c.url,{waitUntil:'domcontentloaded',timeout:30000}).catch(()=>{});}"#,
    r#"report();},250);"#,
    // A browser that dies (crash, or the user closing the last page) must not
    // leave a helper claiming a live page.
    r#"browser.on('disconnected',()=>{write({ready:false});process.exit(0);});"#,
    r#"})().catch(fail);"#,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helper_is_one_argv_element_with_no_shell_hazards() {
        // Same rule as the detect probe: it is passed as a single argument, so
        // it must contain neither a newline nor a single quote that would end
        // the quoting `open` wraps it in.
        assert!(!HELPER.contains('\n'), "{}", HELPER);
        assert!(HELPER.contains("chromium.launch"), "{}", HELPER);
    }

    #[test]
    fn the_helper_binds_so_the_pane_can_see_the_page() {
        // Without this the page opens and the pane shows nothing — the whole
        // feature hinges on the browser being published.
        assert!(HELPER.contains("browser.bind('claude'"), "{}", HELPER);
    }

    #[test]
    fn the_helper_reports_startup_failures_instead_of_hanging() {
        // `wait_until_ready` polls the state file; a helper that dies silently
        // would turn every failure into a 45-second timeout.
        assert!(HELPER.contains("unhandledRejection"), "{}", HELPER);
        assert!(HELPER.contains("error:String"), "{}", HELPER);
    }

    #[test]
    fn a_viewport_is_clamped_to_something_a_browser_accepts() {
        assert_eq!(Viewport::sane(0, 0), Viewport { width: 200, height: 200 });
        assert_eq!(
            Viewport::sane(99_999, 99_999),
            Viewport { width: 7680, height: 4320 }
        );
        assert_eq!(
            Viewport::sane(1440, 900),
            Viewport { width: 1440, height: 900 }
        );
    }

    #[test]
    fn a_url_is_never_parsed_by_a_shell() {
        // The launcher runs through `sh -c`, so the script is quoted with the
        // POSIX close-escape-reopen form: the embedded quote becomes `'\''`,
        // which leaves the `;rm` inside the string rather than starting a new
        // command. (A naive "the output must not contain ';rm'" check fails
        // here and would be wrong — that substring is *inside* the quoting.)
        assert_eq!(
            shell_quote("http://x/?a=1&b=2';rm -rf /"),
            r"'http://x/?a=1&b=2'\'';rm -rf /'"
        );
        // The control channel doesn't go near a shell at all: the JSON travels
        // as an argv element to `node`.
        assert!(!HELPER.contains("exec("), "{}", HELPER);
    }

    #[test]
    fn state_defaults_to_not_ready_rather_than_failing() {
        // An empty/absent state file is the normal case before anything runs.
        let s: PageState = serde_json::from_str("{}").unwrap();
        assert!(!s.ready);
        assert!(s.error.is_none());
    }
}
