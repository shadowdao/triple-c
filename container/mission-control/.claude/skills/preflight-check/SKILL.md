---
name: preflight-check
description: Verify all registered projects have current methodology files and crew definitions. Reports status and offers to re-initialize projects that need it.
---

# Preflight Check

Verify all registered projects have current Flight Control methodology files and crew definitions. Reports findings and offers to run `/init-project` on projects that need it.

## When to Use

- After updating methodology files or adding new skills to mission-control
- After adding new default crew files
- Periodically, to catch drift across managed projects
- When onboarding a new team member who may have stale project setups

## Prerequisites

- `projects.md` must exist (run `/init-mission-control` first)

## Workflow

### Phase 1: Load Projects Registry

1. **Read `projects.md`** to get the full list of managed projects
2. Extract each project's slug, path, and description

### Phase 2: Check Each Project

For each project in the registry:

1. **Verify project path exists** — if the directory doesn't exist on disk, mark as `unreachable` and skip
2. **Run the sync check**:
   ```bash
   bash "${SKILL_DIR}/../init-project/check-sync.sh" \
     "${SKILL_DIR}/../init-project" \
     "{project-path}/.flightops"
   ```
3. **Parse the output** and classify the project:

| Status | Meaning | Action Needed |
|--------|---------|---------------|
| `missing` | No `.flightops/` directory | Needs `/init-project` |
| `outdated` | Methodology files differ from source | Needs `/init-project` |
| `current` | Methodology files match | None for methodology |
| `agent-crews:missing` | No crew directory at all | Needs `/init-project` |
| `agent-crews:empty` | Crew directory exists but empty | Needs `/init-project` |
| `crew-missing:{file}` | Specific crew file missing (new skill) | Needs crew file added |
| `legacy-layout:*` | Old directory naming detected | Needs migration via `/init-project` |

### Phase 3: Report

Present a summary table:

> **Project Sync Status**
>
> | Project | Path | Methodology | Crew | Issues |
> |---------|------|-------------|------|--------|
> | my-app | ~/projects/my-app | current | 1 missing | `routine-maintenance.md` not found |
> | api-server | ~/projects/api-server | outdated | current | Methodology files stale |
> | frontend | ~/projects/frontend | current | current | — |
>
> **{N} projects need attention, {M} are current.**

Group issues by type:
- **Needs full init**: projects with `missing`, `agent-crews:missing`, or legacy layouts
- **Needs methodology update**: projects with `outdated` status
- **Needs new crew files**: projects with `crew-missing` entries (list which files)
- **Unreachable**: projects whose paths don't exist on disk

### Phase 4: Remediate

If any projects need attention:

> "Want me to run `/init-project` on the projects that need it?"
>
> Options:
> - **All** — re-init every project that needs attention
> - **Select** — choose which projects to update
> - **Skip** — just take the report, no changes

For each project the user selects:

1. **Run `/init-project`** using the skill workflow (read `.claude/skills/init-project/SKILL.md` and execute)
   - For projects that only need new crew files: skip straight to Step 6 (Configure Project Crew) — the "If exists (re-run)" path will copy missing crew files from defaults without touching existing ones
   - For projects that need methodology updates: run the full workflow
   - For projects with legacy layouts: run from Step 2 (migrations)
2. **Report result** — confirm what was updated for each project

### Phase 5: Summary

After remediation (or if skipped), output a final status:

> **Sync complete.**
> - {N} projects checked
> - {M} projects updated
> - {K} projects already current
> - {J} projects unreachable (verify paths in `projects.md`)

## Guidelines

### Non-Destructive

This skill never overwrites customized files without user consent. The underlying `/init-project` workflow:
- Copies missing crew files from defaults (safe — fills gaps)
- Asks before updating existing crew files (respects customization)
- Never overwrites `ARTIFACTS.md` (project-specific)

### Missing Crew Files vs. Drift

These are distinct situations:
- **Missing crew file**: A new default crew was added to mission-control but the project doesn't have it yet. This is a gap — the file should be copied from defaults.
- **Crew file drift**: A project's crew file differs from the current default. This is expected — projects customize their crews. Report it as informational but do not flag it as needing remediation.

### Quick and Quiet

If all projects are current, say so in one line and stop. No confirmation prompts needed when there's nothing to do.
