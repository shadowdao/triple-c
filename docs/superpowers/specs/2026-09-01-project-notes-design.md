# Project Notes — design

**Date:** 2026-09-01 · **Baseline:** v0.4 · Companion to [ROADMAP.md](../../../ROADMAP.md)
and [DESIGN-REVIEW.md](../../../DESIGN-REVIEW.md).

A per-project notes surface, with a per-note **Send to agent** action that puts the note
into a running Claude session's prompt.

---

## Why this earns a slot

DESIGN-REVIEW's coherence test says every screen answers exactly one question. Notes
answers *"what do I want to hand this agent, and what did I keep learning here?"* — and it
answers it **while the container is stopped**, which is the gap the stop/start container
model creates and the same reasoning that made Sessions/Resume the flagship.

Two things already do part of this job, and the design is shaped to avoid both:

- `Project.claude_instructions` (`models/project.rs:405`, editor at
  `components/projects/ClaudeInstructionsEditor.tsx`) is per-project free text merged into
  the container's `CLAUDE.md` on every start. It is **ambient** — always in context, never
  addressed. Notes are **discrete and fired on demand**. If Notes drifts into a second
  instructions box, it is redundant with a feature that already ships.
- A `NOTES.md` in the workspace is readable by the agent already, but unreadable by the
  user when the container is stopped, and invisible to the fleet view.

What Notes uniquely adds is *addressable items with a fire-at-the-session action*.

## Decisions taken

| Decision | Choice | Rationale |
|---|---|---|
| Audience | Human scratchpad **and** agent prompts, one surface | See "no note types" below |
| Storage | Own file per project, host-side | Keeps prose out of `projects.json`; works with the container stopped |
| Send target | Project's own sessions; picker when >1 | Never guesses; mirrors STT's target-pinning guard |
| Surface | Side dock that takes space **inward**; never resizes the OS window | Phase 0 spike: growing corrupts under native Wayland, §6.1 |
| Formatting | Plain text, no markdown | It is a scratchpad; see §3 |
| Tab position | Last, after Browser | A companion to the work, not a step in it |

**No note *types*.** A note is a title plus a body. What makes one "for the agent" is that
you pressed the button, not a mode set at creation. The moment there is a "prompt note" vs
"scratch note" toggle, the pane is two features wearing one coat, and every note costs a
classification decision at the moment of writing — which is the moment the user is least
willing to make one.

---

## 1. Storage

New `app/src-tauri/src/storage/notes_store.rs`, modeled on `migration_store.rs` rather than
on `projects_store.rs`:

```
<data_dir>/triple-c/notes/{project_id}.json
```

- **`sanitize()` on the project id**, copied from `migration_store.rs:41-46`. The id arrives
  over IPC; it must not be able to steer the write.
- **Atomic *and durable* write** — `.tmp`, `sync_all()`, `rename()`, then fsync the
  directory, per `migration_store.rs:203-261` rather than `projects_store.rs:167-179`. That
  file's comment is explicit that write-temp-then-rename alone is only half of it: `fs::write`
  returns once the bytes are in the page cache, so losing power in the window leaves the
  rename applied and the data not written — a truncated file produced by the very code meant
  to prevent one. Notes are user prose; that is the data least worth losing to a half-write.
- **Corrupt file is copied aside and left in place**, per `migration_store.rs:49-125` —
  timestamped, capped, and never overwriting an earlier copy, because the first copy is the
  one taken before anything rewrote the file.
- **Path resolution is split for testability.** `dirs::data_dir()` is resolved in thin public
  wrappers; the real work takes an explicit `&Path`. `ProjectsStore::new()` hardcodes
  `dirs::data_dir()` and is therefore not constructible against a temp dir, which is why its
  own tests only exercise free functions. The notes store should not inherit that limit.

```rust
struct Note {
    id: String,          // uuid v4
    title: String,
    body: String,
    pinned: bool,
    created_at: String,  // RFC 3339
    updated_at: String,
}
struct ProjectNotes { version: u32, notes: Vec<Note> }
```

Order is pinned-first then `updated_at` descending. Manual reordering is deliberately out.

### Why not a field on `Project`

`projects.json` is written on **every blur** by the debounced `useProjectSave` path
(`hooks/useSaveState.ts`, threaded through `ProjectHome.tsx:79-81` into Overview and
Config). Long user prose on that record means (a) the whole project list is rewritten every
time a note changes, and (b) a note edit and a Config edit can race, with the loser's write
clobbering the winner's. `migration_store.rs:1-12` already documents this exact reasoning
for why *it* is not in `projects.json`. Notes inherit it.

A per-project file also means a corrupt notes file loses notes for one project, not the
project list.

### Lifecycle

`remove_project` deletes the project's notes file. A failure there is logged, never fatal —
an orphaned notes file is harmless, a project that cannot be removed is not.

## 2. Commands and frontend state

Registered in `lib.rs` via `generate_handler!`. Per CLAUDE.md, application commands need
**no** entry in `capabilities/default.json`.

- `list_notes(projectId) -> Vec<Note>`
- `save_note(projectId, note) -> Note` — upsert; stamps `updated_at` backend-side
- `delete_note(projectId, noteId)`

There is deliberately **no whole-list setter**. Bulk writes are the clobbering mechanism the
storage choice above exists to avoid.

`notes_store` is a **free-function module** keyed by project id, exactly like
`migration_store` — no struct, nothing held in `AppState`, no in-memory copy of the notes.
`ProjectsStore`'s `Mutex` exists because it caches the project list in memory; a notes store
that reads and writes the file per call has nothing to cache and nothing to guard. What it
does need is that each upsert's read-modify-write is not interleaved with another's, so the
module holds one process-wide write lock (`OnceLock<Mutex<()>>`, the idiom already in
`browser_view/popout.rs`) taken for the read-modify-write, not for the read path.

Frontend: wrappers in `lib/tauri-commands.ts`, a `hooks/useNotes.ts`, and notes cached in
zustand keyed by project id. Rust is the source of truth; the cache is a cache.

The dock and the tab live in one webview, so zustand alone suffices. The store boundary is
drawn so that a future detached window (§8) only swaps the transport: Rust emits a
`notes-changed` event, both windows listen.

## 3. Editor — plain text, deliberately

A note is a title and a `<textarea>`, saved on blur, following
`ClaudeInstructionsEditor.tsx` (which saves in `onBlur` and holds no timer) and reporting
the outcome through `ui/SaveIndicator`. There is no debounce anywhere in the existing save
path — `useSaveState.ts`'s only timer is a 2500 ms reset of the "Saved ✓" label — and notes
add none. **No rich editor, no markdown library, and no markdown
rendering** — it is a scratchpad for reminders, and it stays one.

The body is stored and displayed exactly as typed. There is no view/edit mode split, so
there is no state to get wrong and no moment where the text the user is looking at is not
the text that would be sent.

This also keeps `renderMarkdown()` (`components/layout/HelpDialog.tsx:55-160`) where it is.
It was written for Help content, it entity-escapes before converting to make its
`dangerouslySetInnerHTML` sink safe, and `HelpDialog.test.tsx` asserts that escaping as a
security rule. Reusing it here would mean extracting it and giving a hand-rolled HTML
converter a second caller with different content — cost and risk, for formatting a
scratchpad. If notes later need rendering, that extraction is the change to make; it is not
this change.

One consequence worth stating: the text sent to the agent is byte-for-byte what is in the
box. Nothing is transformed on the way out except the newline substitution in §5.

## 4. The Notes tab

- One entry in the `TABS` registry (`components/projects/home/ProjectHome.tsx:24-33`), one
  line in the panel switch (`:237-257`), one new `home/NotesTab.tsx` taking the sibling prop
  shape `{ project: Project }`.
- Order: Notes goes **last**, after Browser — **Overview / Sessions / Automation / Config /
  Files / Browser / Notes**. It is a companion to the work, not a step in it, and the
  existing order runs roughly from "what is this" to "what is in it".
- Layout is master/detail: title list left, editor right.

Note that the active sub-tab is local `useState` (`ProjectHome.tsx:47`) and is not
persisted, so a closed and reopened home tab returns to Overview. Notes inherits that; it is
not worth changing here.

## 5. Send to agent

### The newline problem, and why it is already solved

A dictated STT phrase has no newlines. A note body does. Typed as raw keystrokes, every
`\n` in a body **submits a separate prompt** — the note would arrive as N truncated
messages.

The answer is in the codebase already. `components/terminal/TerminalView.tsx` (~:400-430)
handles Shift+Enter by sending `\x1b\r`, and its comment states these are "the in-band
bytes, not a guess," with an explicit warning **not** to simplify to `\n` because a shell
would *run* the line. So:

```
payload = note.body.replace(/\r?\n/g, "\x1b\r")
```

sent with **no trailing CR** — the user presses Enter. Same rationale as STT sending
without one: a note is longer than a dictated sentence, so the chance of wanting an edit
before firing is higher, and an unsent prompt is recoverable while a sent one is not.

Two consequences follow from that same comment:

1. **Only `sessionType === "claude"` sessions are offered as targets.** `bash -l`'s readline
   has no binding for `\e\r` and answers with a bell. Bash tabs are not listed in the picker
   at all.
2. **The sequence lives in one shared helper**, not a second `"\x1b\r"` literal. The
   knowledge in that comment is hard-won and must not be duplicated away from it.

### Target resolution

`TerminalSession` (`lib/types.ts:228-234`) already carries `projectId`, `projectName`,
`sessionType` and `sessionName`, so no new plumbing is needed.

| Claude sessions for this project | Behavior |
|---|---|
| 0 | Button disabled, "no running session for this project" |
| 1 | Send |
| >1 | Menu of session display names (`Project.renamed_session_names` where set) |

The display-name rule is currently written **twice**, both copies non-exported and local to
`MainTabs.tsx` — `tabLabel` (:192-203) and inline in `renderTab` (:362-367). The picker would
be a third copy of a rule that already disagrees with itself the moment one copy is edited,
so it is extracted once to a shared helper and both existing sites call it. That is a
targeted improvement to code this feature depends on, not unrelated refactoring.

- **The target is pinned at click time**, per the hazard `useSTT.ts:20,30` guards against
  (it pins at record-start so text does not land in whatever tab is active at stop time).
- Transport is `useTerminal`'s module-scoped ordered queue (`hooks/useTerminal.ts:32-85`,
  exposed as `sendInput` at `:135-141`) → `terminal_input` → `exec_manager.send_input`. That
  queue exists because parallel `invoke`s raced the session mutex and reordered keystrokes
  (`useTerminal.ts:7-31`); a multi-line note is exactly the payload that would expose it.
- After sending, switch the active tab to that terminal so the user watches it land. This is
  a courtesy, not a correctness requirement: if it cannot be delivered, the send still
  succeeded.
- **Body only, not the title.** The title is an index label for the list, not content.

Explicitly *not* reused: `useProjectActions.ts:104-124`'s `openTerminalWithCommand`, which
opens a shell then types after a `setTimeout(700)`. Starting a container as a side effect of
clicking a note is too large an implicit action, and that timing hack should not spread.

## 6. Surface — a dock that takes space inward

`components/layout/NotesDock.tsx`, a flex sibling of the tab panels in `App.tsx:139-160`, so
it is visible over **any** top-level tab including Terminal. This is the point of the dock:
Project Home and Terminal are sibling top-level tabs (`layout/MainTabs.tsx`), so a
Notes-only-as-sub-tab design hides notes exactly when the agent is running.

Opening the dock **takes space from inside the window**. The terminal narrows and reflows;
the OS window is never resized or moved. `TerminalView.tsx:643-656` already has a
rAF-throttled `ResizeObserver` that calls `fitAddon.fit()` then `resize(sessionId, cols,
rows)` → `terminal_resize`, so narrowing reflows xterm *and* resizes the container PTY
correctly, with no new code.

- Width is drag-resizable, persisted to a `triple-c.notes.dock` localStorage key. Precedent:
  `triple-c.sidebar.collapsed` (`store/appState.ts:4-20`) is the app's only such key today.
- **The dock follows the active tab's project** — terminal tab → that session's project,
  home tab → that project, nothing active → empty state. `activeTabKey`/`tabKeyId` plus
  `TerminalSession.projectId` already provide this.
- **No window geometry code at all.** No `set_size`, no `set_position`, no monitor work-area
  arithmetic, no platform checks. §6.1 is why.

### 6.1 Why the dock does not widen the window — Phase 0 spike

The original design had the dock "expand outward" by widening the OS window, so the terminal
kept its size. A throwaway Tauri app (`geo-spike`) was built and run on the target desktop —
KDE Plasma, Wayland session, 2026-09-01 — because the app contains no window-geometry code
today and the behavior could not be predicted. It was run twice, once per GDK backend,
which turned out to matter more than the platform.

**Under XWayland** (what every Tauri AppImage gets, because `linuxdeploy-plugin-gtk` exports
`GDK_BACKEND=x11` in `AppRun`, citing
[tauri-apps/tauri#8541](https://github.com/tauri-apps/tauri/issues/8541)) everything worked:

| Test | Result |
|---|---|
| Grow while floating | asked +420, got +420 — exact |
| Shrink back | asked -420, got -420 — exact |
| `outer_position()` | readable, correct |
| `work_area` | 3840x2099 — correctly excludes the 61px Plasma panel |
| Grow while maximized / fullscreen | ignored, as designed |

**Under native Wayland** (what the `.deb` and `.rpm` builds get, since they carry no such
hook) the same binary failed — and failed *silently*, which is the part that decided this:

| Test | Result |
|---|---|
| Grow while floating | asked +420, got **+600**; height moved **+276 unrequested** |
| Shrink back | asked -420, got **-240**; height **+276** again |
| After unmaximize | window reports **5400x2900 on a 4800x2700 monitor** |
| `outer_position()` | returned `Ok(0,0)` — for a window that was not at 0,0 |
| Grow while maximized / fullscreen | ignored, as designed |
| `set_position` | ignored, as expected |

Two independent failures, either one sufficient:

1. **Resize compounds.** Under Wayland GTK owns the frame and shadows; both `outer` and
   `inner` report a 0x0 decoration, so every read-back is inflated by a fixed offset and
   every write built on a read-back compounds it. There is no size that can be read and
   safely written back. Three calls in, the window is larger than the display.
2. **Position is a confident lie, not an honest failure.** `outer_position()` returned
   `Ok(0,0)` rather than an error. A design that treats "cannot determine position" as
   "do not grow" never triggers, because the value looks perfectly valid. The room check
   duly reported `slack: 2820px, VERDICT: Grow` from a false origin.

The second point is what rules out a runtime fallback. A clean failure could have been
handled; a plausible wrong answer cannot be detected from the value itself.

Growing therefore works on one packaging channel and corrupts on another — the split is by
**packaging, not platform**, which is worse than a platform split because two users on
identical hardware and OS would see different behavior. A dock that takes space inward
behaves identically on every backend, OS and package, needs no detection, and reuses a
resize path that is already exercised by every terminal in the app.

**Kept as evidence, not as guidance:** `set_position` was honoured under XWayland. The design
does not move the window and must not start.

## 7. Testing

Vitest + jsdom + React Testing Library for the frontend, `#[cfg(test)]` for Rust, per
CLAUDE.md's Testing section.

**Rust (`notes_store.rs`)**
- `sanitize()` rejects traversal and separator characters in a project id
- atomic write leaves no `.tmp` behind; a crash mid-write leaves the previous file intact
- a corrupt file is moved to `.bak` and the store opens empty rather than erroring
- removing a project deletes its notes file; a delete failure does not fail removal

**Frontend**
- send-target resolution at 0 / 1 / N claude sessions, and that bash sessions are excluded
- the newline transform: a multi-line body becomes `\x1b\r`-joined, with no trailing CR
- the dock's project resolution: terminal tab, home tab, and nothing active
- save-on-blur persists, and dock width round-trips through localStorage

Note the limit `TerminalView.tsx`'s own comment records: jsdom never synthesizes the
follow-up keypress, so keyboard-path bugs of that family are invisible to unit tests. The
send path is a direct `sendInput` call rather than a synthetic keystroke, which sidesteps
that — but anything touching real key handling needs a manual check in Chromium.

## 8. Out of scope for v1

- A detached second window for a second monitor. The store boundary in §2 is drawn so it is
  an additive follow-up: emit `notes-changed` from the store's write path, and add a second
  narrowly scoped capability granting the notes window `core:event:allow-listen` /
  `allow-unlisten` — `capabilities/default.json` scopes those to `"windows": ["main"]` today,
  and cross-window sync needs them. Application commands need no ACL entry (CLAUDE.md, Key
  Conventions), so only the events require it. `lib.rs:379-387`'s main-window-only close
  handler would need review at that point.
- Syncing notes into the workspace as `.md` for the agent to read unprompted. There is no
  generic write-a-file-to-container command today (only `write_file_to_container` for image
  paste and `upload_bytes_to_container` for migration), and a second storage path with a
  sync direction is a v2 conversation.
- Tags, full-text search, manual reordering, note history.
- Any change to `claude_instructions`. The two features stay distinct: ambient context
  versus fired-on-demand items.

## 9. Open questions

None. The Phase 0 spike settled the surface (§6.1); every other decision is recorded in the
table above.
