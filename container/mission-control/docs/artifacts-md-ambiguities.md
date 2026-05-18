# ARTIFACTS.md Template — Ambiguities

A review of `.claude/skills/init-project/templates/ARTIFACTS-files.md` (the template copied into each project's `.flightops/ARTIFACTS.md`) against the implicit contract Mission Control skills rely on.

## Context

Mission Control skills make assumptions about what they can read, write, and depend on inside a project. ARTIFACTS.md is the document that's supposed to encode the contract between skills and project-side customization. In practice, the template muddles three different layers:

1. **Mission Control protocol invariants** — fixed across all projects; skills depend on these by name (state values, lifecycle rules, artifact taxonomy).
2. **Project conventions skills must read** — locations, file naming, status field encoding.
3. **Project presentation** — section structure, prose templates, headings; never read by skills.

The template doesn't distinguish between these layers, which causes two failure modes:

- Project owners cannot tell what they can safely customize and what is fixed.
- Skill authors are not steered away from depending on project-owned shape (the most recent example: a draft flight-debrief change that read prior debriefs by `## Test Suite Timing` heading — a heading that lives in the project-owned format and could be removed or renamed at any time).

This document lists the specific ambiguities that motivated the finding.

## Findings

### 1. State values are protocol but presented as project-customizable

The `## State Tracking` table lists `planning → ready → in-flight → landed → completed (or aborted)` as part of the project's local convention. Skills depend on these literal strings:

- `flight-debrief/SKILL.md`: "A flight must have status `landed` before debriefing"; "update the flight artifact's status from `landed` to `completed`"
- `routine-maintenance/SKILL.md`: scaffolds new flights with `Status: ready`

If a project owner renames `landed` to `arrived` in their ARTIFACTS.md, skills break silently. The template gives no signal that this list is fixed by the protocol.

### 2. State transition ownership is unspecified

The state diagrams show transitions but don't say who triggers each one:

- `planning → active` for missions: which actor performs this? No skill currently does it.
- `ready → in-flight` for legs: skill or human?
- `landed → completed`: `flight-debrief` does this conditionally on user confirmation.

A project owner reading the template cannot tell which transitions they own vs which the skills handle.

### 3. "Format" is overloaded

Skills routinely say "use the format defined in `.flightops/ARTIFACTS.md`." The template conflates three different things under that one word:

- **Locations** (`missions/{NN}-{slug}/mission.md`) — skills genuinely depend on these
- **Status field encoding** (e.g. `**Status**: planning`) — skills need to read this
- **Section structure** (`## Outcome`, `## Context`, etc.) — skills should NOT depend on this

There is no rule stated anywhere that skills read locations and status fields but never read by section heading. Without that rule, every new skill edit risks coupling itself to project-owned section structure.

### 4. Conventions section mixes protocol invariants with format choices

In `## Conventions` (lines 556–562 of the template):

- "Never modify legs once `in-flight`" — protocol invariant; project owners cannot opt out
- "Flight logs are append-only during execution" — protocol invariant
- "Mission as briefing: mission.md serves as both" — project choice (a different project might have a separate briefing file)

A project owner cannot tell which lines are invariants vs which are policies they accepted at init time and could revise.

### 5. The taxonomy itself isn't declared fixed

The artifact types — Mission, Flight, Leg, Debrief, Log, Briefing, Maintenance Report — are protocol concepts. Skills speak their names. But the template is structured as if it's describing one project's choices ("This project stores Flight Control artifacts as markdown files"), which invites a reader to think they could rename "Flight" to "Sprint" or drop "Leg" entirely.

### 6. Status field placement is ambiguously specified

"States are tracked in the frontmatter or status field of each artifact." Skills need a deterministic read path. If a project mixes the two — frontmatter for missions but `**Status**:` lines for flights — every skill that reads status must handle both. The template offers a choice without forcing the project to commit to one.

### 7. Template placeholders read ambiguously

`{NN}`, `{slug}`, `{Title}` — are these substituted by skills at artifact-generation time, or by the project owner during init configuration? They are the former, but the file presents them as if they are placeholders awaiting human substitution. A naive project owner might "fill in" `{slug}` with a literal value.

### 8. The init-project / migrations contradiction

`init-project/SKILL.md` says ARTIFACTS.md is project-specific and "Never overwrite — it may contain customizations." But `init-project/migrations.md` describes Mission Control modifying ARTIFACTS.md to update state definitions. Mission Control reserves the right to edit a "project-owned" file when its own protocol changes. The template does not disclose this boundary.

### 9. No statement of the skill/project contract

The largest gap. Nowhere does the template say *what skills will and will not do with this file*. Adding a preamble that explicitly states the contract would resolve most of the issues above:

> Mission Control skills read this file for: artifact locations, file naming, and status field encoding. Skills do not read artifacts by section heading. The State Tracking values and lifecycle rules are fixed by Mission Control and should not be edited. Everything else is yours to customize.

## Proposed direction (not yet implemented)

A small restructure of the template that sorts every line into one of three buckets:

- **`[protocol: do not edit]`** — state values, lifecycle invariants, artifact taxonomy
- **`[project: skills read this]`** — locations, file naming, status field encoding
- **`[project: presentation only]`** — section structure, prose, headings

Plus a preamble that names the skill/project contract explicitly. This clarifies the boundary in the document where new project owners and new skill authors actually look.
