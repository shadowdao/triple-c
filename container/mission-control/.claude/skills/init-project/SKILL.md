---
name: init-project
description: Initialize a project for Flight Control. Creates .flightops directory with methodology reference and artifact configuration. Run before using other Flight Control skills on a new project.
---

# Project Initialization

Prepare a project for Flight Control by creating the `.flightops/` directory with methodology references and artifact configuration.

## When to Use

Run `/init-project` when:
- Starting to use Flight Control on a new project
- You suspect the .flightops reference may be outdated
- Another skill indicates the reference needs to be synced

## Workflow

### 1. Identify Target Project

1. **Read `projects.md`** to find the project's path, remote, and description
2. If the project isn't listed, ask the user for:
   - Project name/slug
   - Filesystem path
   - Brief description
3. Optionally offer to add the project to `projects.md`

### 2. Check and Apply Migrations

Check for legacy directory layouts and offer to migrate them.

1. **Read `migrations.md`** from the skill directory (`${SKILL_DIR}/migrations.md`)
2. **Run detection checks** for each migration in order (001, 002, ...)
3. **If no migrations are needed**, proceed silently to the next step
4. **If any migrations are needed**, present a summary to the user:
   > "Detected legacy directory layout in {project}. The following migrations are available:"
   >
   > - _Each applicable migration's user message_
   >
   > "Apply these migrations?"
5. **On confirmation**, apply the actions for each applicable migration in order
6. **On decline**, warn the user that some skills may not work correctly with the old layout, but continue using whatever directory structure exists

### 3. Check Sync Status

Run the hash comparison script to determine sync status:

```bash
bash "${SKILL_DIR}/check-sync.sh" \
  "${SKILL_DIR}" \
  "{target-project}/.flightops"
```

The script outputs one of:
- `missing` - Directory doesn't exist in target project
- `outdated` - Directory exists but files differ from source
- `current` - All files are up-to-date

### 4. Prompt and Sync Methodology Files

Based on the status:

**If `missing`**:
> "Flight operations directory not found. Create `{project}/.flightops/` with methodology references?"

**If `outdated`**:
> "Flight operations references in {project} are outdated. Update?"

**If `current`**:
> "Flight operations references are up-to-date in {project}."

If the user confirms, create/update the directory:

```bash
mkdir -p "{target-project}/.flightops"
cp "${SKILL_DIR}/FLIGHT_OPERATIONS.md" "{target-project}/.flightops/"
cp "${SKILL_DIR}/README.md" "{target-project}/.flightops/"
```

### 5. Configure Artifact System (New Projects Only)

**Only if ARTIFACTS.md doesn't exist**, ask the user to select an artifact system:

> "How should mission, flight, and leg artifacts be stored?"

Available templates:
- **files** — Markdown files in the repository (`templates/ARTIFACTS-files.md`)
- **jira** — Jira issues: Epics, Stories, Sub-tasks (`templates/ARTIFACTS-jira.md`)

#### 5a. Check for Setup Questions

After the user selects a template, read the template file and check if it contains a `## Setup Questions` section with a table of questions.

If setup questions exist:
1. Parse the questions from the table (first column contains the questions)
2. Ask the user each question interactively
3. Replace the placeholder answers in the table with the user's responses

#### 5b. Copy and Populate Template

Copy the selected template, with answers populated if setup questions were asked:

```bash
cp "${SKILL_DIR}/templates/ARTIFACTS-{selection}.md" \
   "{target-project}/.flightops/ARTIFACTS.md"
```

If setup questions were answered, update the ARTIFACTS.md file to replace the placeholder answers with the user's responses.

**If ARTIFACTS.md already exists**, do not modify it — it's project-specific and may have been customized.

### 6. Configure Project Crew

Set up phase-specific crew definitions that control how Mission Control interacts with project-side agents.

1. **Check if `.flightops/agent-crews/` exists**

   **If missing** (first run):
   - Copy all defaults from `${SKILL_DIR}/defaults/agent-crews/` to `{target-project}/.flightops/agent-crews/`
   - Brief the user:
     > "Default crew has been set up for all phases. Your agent crews define who Mission Control (Flight Director) works with during each phase — which agents exist, their roles, models, and prompts."
   - Ask about customization:
     > "Want to customize any agent crew? (Most projects work fine with defaults)"
   - If yes: ask which crew → show current definitions → walk through changes (add/remove/modify roles, adjust prompts, change interaction protocol)
   - If no: proceed with defaults

   **If exists** (re-run):
   - Copy any missing crew files from defaults (new crews added to the methodology)
   - Ask the user:
     > "Agent crew files already exist. Default crew definitions may have been updated since your project was initialized. Want to review and update any crew files to the latest defaults?"
   - If yes: for each file, show what changed between their version and the current default, ask whether to overwrite or keep their version
   - If no: leave all existing files untouched

### 7. Update CLAUDE.md

Check if the project's `CLAUDE.md` file references the flight operations directory:

1. **If CLAUDE.md doesn't exist**, create it with a Flight Operations section
2. **If CLAUDE.md exists but lacks a Flight Operations section**, append one
3. **If CLAUDE.md already has a Flight Operations section**, leave it unchanged

#### 7a. Fix Stale Path References

If migrations were applied in Step 2, scan the project's `CLAUDE.md` for stale path references within the Flight Operations section and fix them:

- Replace `.flight-ops/` → `.flightops/`
- Replace `phases/` → `agent-crews/` (only within Flight Operations context)

This ensures the CLAUDE.md instructions point to the correct post-migration paths.

Add this section:

```markdown
## Flight Operations

This project uses [Flight Control](https://github.com/msieurthenardier/mission-control).

**Before any mission/flight/leg work, read these files in order:**
1. `.flightops/README.md` — What the flightops directory contains
2. `.flightops/FLIGHT_OPERATIONS.md` — **The workflow you MUST follow**
3. `.flightops/ARTIFACTS.md` — Where all artifacts are stored
4. `.flightops/agent-crews/` — Project crew definitions for each phase (read the relevant crew file)
```

### 8. Post-Sync Instructions

After creating or updating the directory, inform the user:

> "If you have Claude Code running in {project}, restart it to pick up the new flight operations references."

This ensures Claude Code loads the new files into its context when working in the target project.

## Output

This skill creates/updates the following at project root:

```
{project}/
├── CLAUDE.md                  # Updated with Flight Operations section
└── .flightops/               # Hidden directory for Flight Control
    ├── README.md              # Explains the directory purpose
    ├── FLIGHT_OPERATIONS.md   # Quick reference for implementation (synced)
    ├── ARTIFACTS.md           # Artifact system configuration (project-specific)
    └── agent-crews/           # Project crew definitions (project-specific)
        ├── mission-design.md
        ├── flight-design.md
        ├── leg-execution.md
        ├── flight-debrief.md
        ├── mission-debrief.md
        └── routine-maintenance.md
```

## File Sync Behavior

| File | Synced on update? | Notes |
|------|-------------------|-------|
| CLAUDE.md | Append only | Adds Flight Operations section if missing |
| README.md | Yes | Methodology reference |
| FLIGHT_OPERATIONS.md | Yes | Methodology reference |
| ARTIFACTS.md | No | Created once from template, then project-specific |
| agent-crews/*.md | Ask on re-run | Created from defaults; on re-run, user can choose to update to latest defaults |

## Guidelines

### Don't Over-Prompt

If everything is `current`, just inform the user briefly and move on. No confirmation needed.

### Respect ARTIFACTS.md

Never overwrite ARTIFACTS.md — it may contain project-specific customizations. Only create it if missing.

### Keep It Quick

This is a setup step, not the main work. Complete it efficiently so the user can proceed to their actual task.
