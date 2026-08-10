# Triple-C Roadmap — Claude Code Feature Parity

**Date:** 2026-08-09 · **Baseline:** v0.3.0 · **Claude Code reference:** 2.1.226

Companion to [DESIGN-REVIEW.md](DESIGN-REVIEW.md), which covers visual design and
information architecture. This document covers *which Claude Code capabilities Triple-C
should surface, and why.*

---

## Guiding principle

> **Triple-C shows state and launches things. Claude Code edits its own config.**

Triple-C's built-in MCP server management was removed in this cycle because Claude Code
absorbed the capability natively (`claude mcp add/list/remove`, `.mcp.json`, `/mcp`).
Hooks, skills, agents, plugins, output styles, and statusline are the same species: files
under `.claude/` with first-class Claude Code TUIs. Building GUI form editors for them
means losing the same race again.

What Claude Code cannot do is what Triple-C uniquely owns: **the container boundary and
what persists behind it** — the config volume, workspace mounts, lifecycle, the bundled
scheduler, and the fleet view across many projects.

---

## Current coverage (v0.3.0)

Triple-C sets exactly five `settings.json` keys, plus a sandbox block:

| Key | Surfaced as |
|---|---|
| `tui` | TUI Mode select (`fullscreen`) |
| `effort` | Effort Level select (`low`/`medium`/`high`) |
| `autoScrollEnabled` | Auto-Scroll Disabled toggle |
| `focusMode` | Focus Mode toggle |
| `showThinkingSummaries` | Thinking Summaries toggle |
| `sandbox.*` | Sandbox toggle (`enabled`, `enableWeakerNestedSandbox`, `allowUnsandboxedCommands`) |

Plus four env feature flags — `CLAUDE_CODE_NO_FLICKER`, `CLAUDE_CODE_ENABLE_AWAY_SUMMARY`,
`CLAUDE_CODE_SUBPROCESS_ENV_SCRUB`, `ENABLE_PROMPT_CACHING_1H` — and arbitrary user-set
`CLAUDE_CODE_*` vars via the Env Vars modal.

Also covered: per-project auth backends (Anthropic OAuth, Bedrock incl. SSO refresh,
Ollama, OpenAI-compatible), user-level `CLAUDE.md` composition, `claude update` on every
container start, terminal ergonomics (OAuth URL detection, OSC 52 clipboard, image paste,
file drag-drop, STT), the web terminal, and workspace backup.

---

## Gap analysis

### Committed for this cycle

| # | Gap | Today | Plan |
|---|---|---|---|
| 1 | **Permission modes** | one boolean → `--dangerously-skip-permissions` | Four-state control (Plan / Default / Accept Edits / Bypass) → `--permission-mode`. Verified choices on 2.1.226: `acceptEdits`, `auto`, `bypassPermissions`, `manual`, `dontAsk`, `plan`. |
| 2 | **Session resume** | none | List sessions from the config volume; `[Resume]` opens a terminal on `claude --resume <id>`. |
| 3 | **Capability inventory** | none | Read-only counts + names for skills / agents / hooks / plugins / commands / native MCP servers. Deep-link to the terminal to manage. |
| 4 | **Automation** | `triple-c-scheduler` ships in every container with *zero* UI | Task list, cron editor, run-now, logs, notification badges. |
| 5 | **Container auth handoff** | manual code paste | See "Authentication handoff" below — design decision pending. |

### Deliberately skipped

Status line builder · output-styles editor · hook *editors* · checkpoint/rewind browser ·
plugin marketplace browser. Each is niche, natively handled by Claude Code's own TUI, or a
settings-editor trap. Surface counts and deep-link instead.

### Not yet scheduled

- Granular `permissions.allow` / `ask` / `deny` rules and `additionalDirectories`
- Sandbox detail settings (`filesystem.allowRead/allowWrite`, `allowedDomains`,
  `excludedCommands`) — currently documented for hand-editing via `SANDBOX_INSTRUCTIONS`
- Project-level `.claude/settings.json` vs user-level settings hierarchy
- A model picker. **Note:** the only model strings in the app today are stale placeholders
  (`anthropic.claude-sonnet-4-20250514-v1:0` in `AwsSettings.tsx` and `ProjectCard.tsx`,
  `qwen3.5:27b`, `gpt-4o / gemini-pro / etc.`). These are free-text placeholders, not
  dropdowns, but they should be refreshed to current model identifiers regardless.
- The container's settings.json merge is **shallow** (`jq -s '.[0] * .[1]'`), so a
  user-authored nested block such as `sandbox.filesystem.allowWrite` is replaced wholesale
  on every container start. Worth deepening to `*` recursive merge.

---

## Authentication handoff

**Goal:** stop making users hand-copy an auth code into every container.

**Constraint discovered during research:** `claude login`'s callback server uses an
**ephemeral port** and its redirect URI is **not configurable** for the main login flow
(`--callback-port` and `oauth.callbackPort` apply to *MCP server* OAuth only). So a design
that pre-assigns each container a fixed callback port and routes to it cannot work as
stated — there is no fixed port to route.

There is also a known container gotcha: on Linux, Node resolves `localhost` to IPv6 first,
so the callback server may bind `[::1]:PORT` only and be unreachable over IPv4
([anthropics/claude-code#44844](https://github.com/anthropics/claude-code/issues/44844)).

Two viable options:

### Option A — long-lived token injection (simple)

`claude setup-token` (verified present on 2.1.226: *"Set up a long-lived authentication
token (requires Claude subscription)"*) returns a ~1-year OAuth token. Triple-C runs it in
a running container, stores the token in the OS keychain via the existing `secure.rs`, and
injects `CLAUDE_CODE_OAUTH_TOKEN` into every container on the Anthropic backend.

**Correction to an earlier assumption in this document.** `setup-token` does *not* start a
loopback callback listener, so it does not need the Auth Bridge. Verified by running it
under a pty: its `redirect_uri` is Anthropic-hosted
(`https://platform.claude.com/oauth/code/callback`), the user copies a code off that page,
and the CLI blocks at a `Paste code here if prompted >` prompt on **stdin**. A stdin path
is therefore mandatory — the flow cannot complete without one.

- No routing, no ports, no proxy.
- One auth event covers every project.
- Cost: small. Reuses existing keychain and env-injection plumbing.
- Limits: token is subscription-scoped and expires annually; per the docs a `setup-token`
  token cannot drive Remote Control sessions or claude.ai connector fetches.

Change detection uses a **random rotation id** in the `triple-c.claude-token-version`
label, not a hash of the token. Labels are readable by anything that can run
`docker inspect`, so a hash would be an offline verification oracle — given a candidate
token you could confirm it. A presence boolean would instead miss rotations and silently
leave containers on a stale token.

### Option B — the Auth Bridge (general loopback-callback bridge)

Option A only solves Claude Code. The same problem affects every CLI that authenticates by
starting a temporary loopback listener and opening a browser at a URL that redirects back
to it — Concourse `fly login` (random loopback port serving `/auth/callback`),
`aws sso login`, and many others. Inside a container the host browser cannot reach that
listener, so login stalls.

Because the ports are ephemeral and unconfigurable, nothing can be pre-assigned. The bridge
**discovers** listeners instead:

1. While enabled for a running project, poll the container for loopback TCP listeners by
   reading `/proc/net/tcp` and `/proc/net/tcp6` over `docker exec` — no dependency on
   `ss`/`netstat`/`lsof`, which aren't guaranteed in the image.
2. For each newly-appeared loopback listener, bind **the same port on the host's
   `127.0.0.1`** (never `0.0.0.0` — that would expose container internals to the LAN).
3. Proxy each accepted connection into the container over the Docker API via
   `socat - TCP:127.0.0.1:<port>` (socat already ships in the image), reusing the existing
   attached-exec streaming in `docker/exec.rs`. Going through the Docker API rather than a
   container IP keeps this working on Docker Desktop, where container IPs are not routable
   from the host.
4. Fall back to `TCP6:[::1]:<port>` when the listener appeared only on IPv6 — on Linux,
   Node resolves `localhost` to IPv6 first, so `claude login` frequently binds `::1` only
   ([anthropics/claude-code#44844](https://github.com/anthropics/claude-code/issues/44844)).
5. Tear down when the listener vanishes, the container stops, the bridge is disabled, or
   the app exits. Ports already covered by the project's explicit port mappings are skipped;
   host-side conflicts are reported rather than silently swallowed.

Opt-in per project (`auth_bridge_enabled`, default off), since it makes container-internal
loopback services reachable from the host.

**Plan:** ship **A** for Claude Code specifically — it removes the pain for the common case
at a fraction of the cost — and **B** as the general mechanism covering every other CLI.
They compose: A means most users never trigger a browser login at all; B catches AWS SSO,
Concourse, and anything else that needs a real callback.

---

## Sequencing

**Phase 0 — done.** Remove MCP (frontend, backend, entrypoint, docs) with a self-healing
migration for containers created against the old per-project Docker network.

**Phase 1 — foundations.** Permission modes end-to-end (including the scheduler bug fix
below). Read-only introspection backend: sessions, capabilities, scheduler.

**Phase 2 — Tier-1 polish.** Focus rings, contrast fixes, real buttons, inline start/stop
progress, status labels, onboarding welcome screen, shared accessible `<Modal>`.

**Phase 3 — Project Home.** Move project config out of the sidebar card into a tabbed
main-area view (Overview / Sessions / Automation / Config), dissolving the modal pile and
splitting the 1,257-line `ProjectCard`.

**Phase 4 — authentication handoff.** Option A, then evaluate B.

**Phase 5 — Library.** Global skills/agents/commands with per-project enable, synced into
the config volume by the entrypoint. Generalizes the pattern the MCP tab was reaching for.

---

## Bugs found during this review

1. **Scheduled tasks ignore the project's permission setting.**
   `container/triple-c-task-runner:69` runs
   `claude -p "$PROMPT" --dangerously-skip-permissions` unconditionally, regardless of the
   project's Full Permissions toggle. Being fixed as part of Phase 1.

2. **Docs claim Reset preserves credentials; it does not.**
   `rebuild_project_container` calls `remove_project_volumes`, which deletes both
   `triple-c-home-{id}` (holding `~/.claude.json`) and `triple-c-claude-config-{id}`
   (holding `~/.claude`). README.md, HOW-TO-USE.md, and CLAUDE.md all still state that
   OAuth tokens survive a Reset. Pre-existing; not yet corrected.

3. **An invalid cron expression silently unscheduled every task.** Found while adding
   task creation to the Automation tab, and the most serious bug in this review.
   `triple-c-scheduler` never validated `--schedule`, and `rebuild_crontab` regenerates the
   *entire* crontab and pipes it to `crontab`, which rejects the whole file if any single
   line is malformed — with the error discarded by `2>/dev/null || true`. So one bad
   schedule silently unscheduled every other task in the container, reporting success.
   Reproduced directly. This mattered because the global CLAUDE.md instructs Claude to use
   this CLI, so Claude itself could trigger it. Fixed at the root: `add` now validates the
   expression and exits non-zero, and `rebuild_crontab` reports a rejected crontab instead
   of swallowing it. The Rust `add_scheduled_task` command validates independently.

4. **Reset was destructive with no confirmation.** It deletes both volumes — the login,
   installed skills, all session transcripts — from a single unconfirmed click, while the
   comparably destructive Remove already confirmed. Now gated by a dialog that names each
   loss. Fixed.

5. **Cancelling authentication did not cancel.** Fixed — see the handoff section above.

6. **Stale model placeholders** — see "Not yet scheduled" above.

7. **Silent save failures.** Project config saves on blur; failures went only to
   `console.error`. Fixed in Phase 3 — `useProjectSave` now renders a
   Saved / Saving / Save failed indicator and raises a toast.

---

## Known gaps left by Phase 2–3

- **Editing a scheduled task changes its id.** `triple-c-scheduler` has no `edit`
  subcommand, and hand-editing its JSON behind its back would desync the crontab, so edit is
  implemented as add-then-remove. The add runs first, so a rejected edit leaves the original
  intact. The task gets a new id and its older logs stay under the old one; the editor says
  so before saving.
- **`open_terminal_session` takes no command argument.** "Resume session" and
  "Manage in terminal" therefore open a bash tab and *type* the command after a
  fixed prompt delay. It works, but it is timing-dependent and will misfire on a
  slow container start. The fix is a `command: Option<String>` parameter on the
  Tauri command so the exec launches the process directly.
- **Uptime is observed, not reported.** `get_container_info` returns a status enum
  with no start time, so Project Home records "running since" when the app *sees*
  the transition. A container already running when the app launches shows
  `● Running` with no elapsed time. Surfacing Docker's `State.StartedAt` would fix it.
- **`lucide-react` was not adopted** (DESIGN-REVIEW Tier-1 #9) — no package-registry
  access in the build environment used for this cycle. The existing inline SVGs and
  text glyphs remain.
- **The tab strip stayed in the TopBar** rather than moving onto the terminal panel's
  top edge. DESIGN-REVIEW §A6 asks for the move but its own §B2 layout diagram puts
  the tabs in the TopBar; the diagram won. Worth revisiting.
- **`Ctrl+Shift+W`, not `Ctrl+W`, closes a tab.** Plain `Ctrl+W` is readline's
  `kill-word`, used constantly inside the terminal this app is built around;
  intercepting it globally would break word-erase in every shell.
