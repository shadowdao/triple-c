# Triple-C Design & Product Review

**Date:** 2026-08-09 · **Version reviewed:** 0.3.0 · **Reviewer:** Fable 5

Scope: `app/src/` (App, layout, projects, settings, terminal, ui, store, index.css),
README/CLAUDE.md/TODO.md, the four repo screenshots, and `triple-c-app-logov2.png`.

---

## Summary verdict

The bones are good. The floating-panel layout reads clean, the GitHub-dark palette is
inoffensive, and terminal-as-centerpiece is correct for this product.

The two real problems are structural, and they are the same problem seen from two sides:
**the project — the app's actual unit of work — has no room to live.** Everything about a
project (backend auth, mounts, git identity, env vars, ports, Claude settings, file
manager) is stuffed into a ~280px sidebar card (`ProjectCard.tsx`, 1,257 lines) that
sprays out seven modals to compensate.

`screenshot_for_fix/project_config_run_off.png` is not a bug to patch. It is the
architecture reporting that the config does not fit where it lives. Fixing that one thing
also solves the modal pile, the density problems, *and* creates the surface where newer
Claude Code concepts belong.

---

## Part A — Visual & interaction design

### A1. Tokens: coherent but thin, with one real contrast failure

`index.css` is GitHub Primer dark, verbatim (`#0d1117 / #161b22 / #21262d / #30363d /
#8b949e / #58a6ff`). Defensible — familiar, calm, terminal-adjacent — but the token layer
stops at 11 variables. Roles the code is already faking ad hoc:

- **No elevation/overlay token.** Modals reuse `--bg-secondary`, so a modal over the
  sidebar is the same color as the sidebar. Add `--bg-overlay: #1c2128` and
  `--shadow-overlay`.
- **No muted-accent tokens.** The code hand-rolls `bg-yellow-500/20 text-yellow-400`,
  `bg-blue-500/20 text-blue-400`, `--warning/15`, `--error/10`. Add `--accent-muted`,
  `--warning-muted`, `--error-muted`, `--success-muted`. Those raw Tailwind palette colors
  are the only two places the token system leaks.
- **Radius drift:** `rounded` (4px), `rounded-lg` (8px), plus hardcoded 3px/6px in help
  styles. Pick two: 6px controls, 8px panels.

**Contrast bug (concrete):** white text on `--accent #58a6ff` is ~**2.5:1** — fails WCAG
AA. That is the primary button ("Add Project"), the "Update" pill, and more. Primer solves
this with two accents: keep `#58a6ff` as the *foreground/link* accent and add
`--accent-emphasis: #1f6feb` for filled buttons (white on `#1f6feb` ≈ 4.7:1).

Same story for `bg-[var(--success)] text-white` ON toggles — `#3fb950` + white ≈ **2.1:1**,
the worst offender in the app.

What passes: `--text-secondary #8b949e` on `#161b22` ≈ 5.8:1, fine even at 12px.
`--warning #d29922` ≈ 7:1. But `disabled:opacity-50` on secondary text drops to ~2.4:1 —
and since the entire config form is disabled while the container runs, **the most common
state of the form is illegible.** Use a dedicated `--text-disabled: #6e7681` instead of
opacity.

### A2. Type and density: everything is 12px

Roughly 90% of the UI is `text-xs`. Hierarchy is carried almost entirely by weight plus a
single `text-lg` modal title. Forms feel cramped rather than dense — density is
information per pixel, not small type.

Proposed scale with roles: **11px** uppercase section labels (already used, keep) ·
**12px** secondary/meta · **13px** default UI/body/form values · **14px** panel headers ·
**16px** view titles.

Path strings in mono are a nice identity touch — extend mono to all machine values (model
IDs, ports, digests), which the Bedrock/Ollama forms currently render in the UI face.

The outer chrome spends generously while content starves: `App.tsx` wraps everything in
`p-6 gap-4`, then the config form gets ~180px-wide inputs for AWS secret keys. Keep the
floating-island look; `p-3 gap-3` buys content ~24px horizontally and the terminal two
more rows.

### A3. The project card is three components wearing one div

`ProjectCard` is simultaneously a list row, a command strip, and the entire settings form.

- **Selection and disclosure are conflated.** Clicking a row both selects it and expands an
  accordion in place, shoving the other projects down. The 06-28 screenshot shows 18
  projects — this jank is daily.
- **Actions are unstyled text links.** `ActionButton` renders `text-xs px-2 py-0.5` colored
  text with no border or background, so Start/Stop/Terminal/Shell/Files/Backup/Config/Remove
  read as a wrapping line of links. Worse, **Remove (destructive, red) wraps directly next
  to Config** with a ~20px hit target.
- **Double-click-to-rename** is undiscoverable and keyboard/touch-inaccessible.
- **27 hover-only `<Tooltip>` markers in ProjectCard alone.** When a form needs 27 tooltips,
  the form is the problem.

### A4. Modals: eight is a pattern smell, and none are real dialogs

Hanging off ProjectCard: EnvVars, PortMappings, ClaudeInstructions, ClaudeCodeSettings,
ContainerProgress, FileManager, ConfirmRemove — plus AddProject, three reused from
SettingsPanel, and Update/ImageUpdate/Help from TopBar.

Each reimplements the overlay div, Escape handler, and click-outside logic by hand. **None
has `role="dialog"`, `aria-modal`, a focus trap, or focus restore** — zero hits for
`role=`, `aria-modal`, or `tabIndex` across `components/`.

The pattern is wrong not because modals are bad, but because these are not modal *tasks*.
Env vars, ports, instructions, and Claude settings are all "edit part of the project
config" — a detail view's job.

- Legitimately modal: **ConfirmRemove**, **AddProject**.
- **FileManager** wants to be a main-area tab, not a 42rem popup.
- **ContainerProgressModal actively hurts:** starting a container blocks the entire app
  behind an overlay for an operation designed to be routine. Replace with inline row state
  plus an error toast.
- Whatever survives should be one shared `<Modal>` primitive with focus trap + ARIA.

### A5. Keyboard and focus: currently unsupported

For a tool whose centerpiece is a keyboard-driven terminal, the chrome is mouse-only.

- Inputs use `focus:outline-none` with only a low-contrast border swap; **buttons have no
  focus style at all** — tabbing through the sidebar is invisible.
- One-line fix: add `--focus-ring: #58a6ff` and
  `:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 1px; }`
- No shortcuts for constant actions: `Ctrl+T` new terminal, `Ctrl+Tab`/`Ctrl+1..9` switch,
  `Ctrl+W` close, `Ctrl+P` project switcher. The only shortcut in the app is the STT mic.
- Hit targets below 24px: tab close "×" (~14px), Tooltip "?" (14px), Browse "...". The
  status bar is `h-6` yet hosts two interactive controls.

### A6. Status communication

Three disconnected dot systems (TopBar Docker/Image, per-project status, StatusBar counts),
all 8px and color-only.

- **Stopped (gray) and error (red) differ only by hue**, and Docker-unavailable renders the
  same gray as Docker-still-being-checked (`dockerAvailable === null` and `false` both fall
  through). An outage should be loud; unknown should pulse.
- Color-only encoding fails colorblind users. Add shape or text — `● Running`, `○ Stopped`,
  `⚠ Error`. The words are already in the model.
- Raw `String(e)` errors dumped into a 12px card line; bollard errors are long. Errors need
  a home: toast plus expandable detail.
- The TopBar tab strip is visually disconnected from the terminal it controls. Move tabs
  onto the terminal panel's top edge so the active tab connects to its content.

### A7. Empty and first-run states

`WelcomeScreen` is three lines of gray text with no affordance — "Add a project from the
sidebar" *describes* a button instead of *being* one. This is also where brand could exist:
the orange sun-gear logo appears nowhere in the UI and shares no DNA with the blue-on-
graphite chrome.

Make it an onboarding checklist reusing state already tracked:
✓ Docker detected → ✓ Image pulled → **[ Add your first project ]** → open terminal.
The same pattern fixes the "image missing" case, today just a gray dot in the corner.

### A8. Dark-only: keep it

Right call. Terminal-first developer tool, xterm content is dark, audience expects it. The
tokens make a light theme cheap later. Don't spend on it now — but keep discipline that no
color bypasses the token layer.

### A9. Iconography

Mixed: hand-inlined Feather-style SVGs in the sidebar rail, text glyphs elsewhere ("×",
"?", "...", "+", "✓", "✕"). Adopt `lucide-react` — same stroke style already being
imitated, tree-shakeable — and replace the text glyphs. It also supplies the per-concept
icons Part B needs.

---

## Part B — Information architecture & product concepts

### B1. The diagnosis

Current IA: `Projects | MCP | Settings` in a sidebar, terminal in main, project detail
crammed into the list.

Deleting the MCP tab was correct — but **the lesson matters more than the freed slot.
MCP died as a Triple-C feature because Claude Code absorbed it.** Hooks, skills, agents,
plugins, output styles, and statusline are all the same species: files under `.claude/`
that Claude Code manages natively with its own TUIs (`/agents`, `/hooks`, `/plugins`). If
Triple-C builds form editors for them, it loses the same race again and becomes exactly
what it should fear — a settings-file editor with a GUI skin.

What Claude Code *cannot* do is what Triple-C uniquely owns: **the container boundary and
what persists behind it.** The config volume, the workspace mounts, the lifecycle, the
scheduler already shipping in every image, and the fleet view across many projects.

> **Principle: Triple-C shows state and launches things. Claude Code edits its own config.**

Sessions, checkpoints, background tasks, scheduled tasks, capability inventory → surface
them, read from the volume, launch into the terminal. Hook/skill/agent *editing* →
deep-link into the terminal, don't rebuild.

### B2. Proposed IA: three nouns

**Project** (a sandboxed workspace) · **Session** (a resumable conversation) ·
**Library** (reusable capabilities pushed into projects). Everything is one of these, or
Settings.

```
┌────────────────────────────────────────────────────────────────────┐
│  TopBar:  ⌂ api-server │ ▣ api-server ✕ │ ▣ api (bash) ✕ │  ● ● ? │
├─────────────┬──────────────────────────────────────────────────────┤
│  ◤ Projects │   MAIN AREA — a tab strip of two tab kinds:          │
│  ● api-serv │   ⌂ project-home tabs   ▣ terminal tabs              │
│  ○ blog     │                                                      │
│  ● data-pipe│   ⌂ api-server                    ● Running · 2h 14m │
│      …      │   ┌─────────┬──────────┬────────────┬────────┐       │
│  ◧ Library  │   │Overview │ Sessions │ Automation │ Config │       │
│  ⚙ Settings │   └─────────┴──────────┴────────────┴────────┘       │
├─────────────┴──────────────────────────────────────────────────────┤
│  StatusBar: 18 projects · 8 running · 4 terminals        🎤  ↓Jump │
└────────────────────────────────────────────────────────────────────┘
```

- **Sidebar** becomes a pure list plus nav rail. Rows carry name, path, status dot, and on
  hover a play/stop and terminal button. Clicking opens (or focuses) that project's
  **Project Home** tab. The freed MCP slot becomes **Library**.
- **Main area** hosts two tab kinds: terminals (as today) and project-home tabs, like VS
  Code's Settings tab. The terminal stays the centerpiece; Project Home is one keystroke
  away rather than a layer on top.
- **All seven config modals dissolve** into the Config tab, full-width, grouped:
  *Workspace* (folders/mounts), *Model* (backend + auth), *Access* (git/SSH/env/ports),
  *Runtime* (docker access, sandbox, permission mode, Mission Control). Room for visible
  helper text kills most of the 27 tooltips. Save-on-blur stays but gains a visible
  "Saved ✓ / Failed" indicator — today failures go only to `console.error`, which is
  silent data loss.

#### Project Home — Overview tab

```
 api-server                                  ● Running · started 2h ago
 [ Stop ] [ Open Claude Terminal ] [ Shell ] [ Files ]        [⋯ menu]

 Permission mode   ( Plan ) ( Default ) ( Accept Edits ) (▮ Bypass ▮)
 Sandbox           ON — bubblewrap isolation           Backend  Anthropic

 CAPABILITIES (read from container volume)
 ◆ Skills 7    ◆ Agents 3    ◆ Hooks 2    ◆ Plugins 1    ◆ Commands 5
   └ click any → drawer listing names/descriptions,
     [Manage in terminal] → opens claude with /agents etc.

 RECENT SESSIONS                              SCHEDULED TASKS
 "Refactor OAuth flow"  2h ago  [Resume]      nightly-review  0 3 * * *
 "Fix flaky CI test"    1d ago  [Resume]      [2 notifications]
```

### B3. The four concepts worth building

**1. Sessions & Resume — the flagship.** The stop/start container model creates a problem
plain Claude Code doesn't have: stop a container, come back Tuesday, and "which
conversation was I in?" is buried in the volume. Read session metadata via `docker exec`
(the exec and tar plumbing already exists), list sessions with summary and age, and make
**[Resume]** open a terminal running `claude --resume <id>`. Closing a terminal tab today
silently abandons a session; it should say "Session saved — resume from Project Home."
This turns the biggest architectural quirk into the best feature.

Do **not** build a checkpoint browser. Mention rewind (`Esc Esc`) in Help and stop there.

**2. Library — the MCP tab's successor.** The pattern was already invented three times:
global MCP servers with per-project checkboxes, global Claude instructions, and Mission
Control's bundled skill install. Generalize it once: a Library of **skills, agents, and
slash commands** defined globally with per-project enable, synced into the container's
`.claude` volume by the entrypoint. Across many projects, "write a skill once, enable it in
twelve sandboxes" is genuinely differentiated. Keep the editor minimal — name plus markdown
textarea, or "import from folder." Not a structured form per frontmatter field.

**3. Permission mode as the hero control.** The whole pitch is "sandbox so you can safely
go fast," yet that pitch is expressed as a scary boolean buried in a config accordion.
Replace it with Claude Code's real vocabulary — a segmented control (**Plan / Default /
Accept Edits / Bypass**) on Overview, echoed as a badge on terminal tabs, with sandbox
state beside it. When sandbox is ON, Bypass loses its red paint ("contained by sandbox");
when sandbox is OFF *and* Bypass is on, that is when caution color earns its place. This
reframes the product's core value in the product's own UI.

**4. Automation tab.** `triple-c-scheduler` ships in every container with
add/list/logs/notifications — and its only UI is a CLAUDE.md paragraph telling Claude to
run it. Wrap it: task list (name, cron, last run, enabled), toggle/run-now/view-log, and a
notification badge on the project row. "Your nightly agent left you a note" is a reason to
open the app in the morning. Fleet-of-scheduled-agents management across projects is
something the Claude Code TUI does not offer.

**Explicitly skip:** status line builder, output-styles editor, hook *editors* (surface the
count, deep-link to the terminal), checkpoint browser, marketplace browser. Each is niche,
natively handled, or a settings-editor trap.

### B4. Coherence test

Every screen answers exactly one question:

| Screen | Question |
|---|---|
| Sidebar | What projects exist and are they up? |
| Project Home | What can this sandbox do, and where did I leave off? |
| Terminal | Do the work. |
| Library | What capabilities do I reuse? |
| Settings | How does the host behave? |

Anything that doesn't answer one of those doesn't get a nav slot.

---

## Priorities

### Tier 1 — high impact, cheap

1. `:focus-visible` ring and stop stripping outlines (one CSS rule + token). Add
   `Ctrl+T` / `Ctrl+W` / `Ctrl+1..9` / `Ctrl+Tab`.
2. Contrast: `--accent-emphasis: #1f6feb` for filled buttons; kill white-on-`#3fb950`;
   `--text-disabled` instead of `opacity-50`.
3. Real buttons for project actions; Remove into an overflow menu; primary action filled.
4. Inline start/stop progress and an error toast; delete `ContainerProgressModal`.
5. Status dots get labels or shapes; Docker-down turns red; null state pulses.
6. Welcome screen becomes an onboarding checklist with a real button, plus the logo.
7. One shared `<Modal>` with focus trap and ARIA for the modals that remain.
8. Permission-mode segmented control replacing the boolean.
9. `lucide-react` icons; move the tab strip onto the terminal panel.

### Tier 2 — high impact, expensive

1. **Project Home tabbed view** — the structural fix that dissolves the modal pile and the
   1,257-line ProjectCard. The forms already exist; this is mostly moving and splitting.
2. **Sessions tab** with `claude --resume`.
3. **Library** — generalize global→per-project sync to skills/agents/commands.
4. **Automation tab** wrapping `triple-c-scheduler`, with notification badges.

### Tier 3 — skip

- Light theme (dark-only is right; tokens keep the door open).
- Editors for hooks, statusline, output styles; checkpoint browser; marketplace browser.
- Any new global sidebar tab beyond Library.
- Rebuilding MCP management in any form. Let the deletion be a lesson, not a vacancy.

---

**One sentence:** promote the project from a sidebar card to a first-class workspace view,
use the volume you already own to surface sessions/capabilities/automation instead of
building config editors, and spend a focused week on focus rings, contrast, and button
affordances — the visual layer needs sanding, not redesign.
