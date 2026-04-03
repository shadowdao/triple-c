---
name: init-mission-control
description: Onboard to Mission Control by setting up the projects registry. Use when projects.md is missing or when adding new projects.
---

# Mission Control Onboarding

Set up the projects registry (`projects.md`) and orient the user to the Flight Control workflow.

## When to Use

Run `/init-mission-control` when:
- `projects.md` doesn't exist (first time using Mission Control)
- Adding a new project to the registry
- Updating an existing project's details

## Workflow

### 1. Check Registry Status

Check if `projects.md` exists in the mission-control repository root.

**If missing:**
> "No projects registry found. Let's set one up — this tells Mission Control where your projects live."

**If exists:**
> "Projects registry found. Want to add a new project or update an existing one?"

### 2. Create or Update Registry

**If creating new:**
1. Copy `projects.md.template` to `projects.md`
2. Remove the example entries
3. Interview the user to register their first project (see step 3)

**If updating:**
1. Read existing `projects.md`
2. Ask: add new project, or update existing?
3. Proceed accordingly

### 3. Discover Projects

Ask the user how they want to add projects:

> "Want to scan a directory for projects, or add a single project?"

**Option A: Scan a directory**
1. Ask for the parent directory path (e.g., `~/projects`)
2. Scan immediate subdirectories for git repos
3. Present the list and ask which ones to register
4. Auto-detect details for each selected project
5. Ask for descriptions in batch (or let the user provide them later)

**Option B: Add a single project**
1. Ask for the project path (e.g., `~/projects/my-app`)
2. Verify it exists and is a git repo
3. Auto-detect details
4. Ask only for what can't be detected: description, and optionally stack/status

**Auto-detection:** For each project directory, run:
- **Slug**: directory name
- **Remote**: `git -C <path> remote get-url origin`

Only ask the user for fields that can't be auto-detected.

### 4. Orientation

After the registry is set up, briefly orient the user:

> "You're all set. Here's the workflow:"
> 1. **`/init-project`** — Run this in each project to set up `.flightops/` with methodology references and configure the project crew
> 2. **`/mission`** — Define what you want to achieve (outcomes, not tasks)
> 3. **`/flight`** — Break a mission into technical specs
> 4. **`/leg`** — Generate implementation steps for each flight leg
> 5. **`/agentic-workflow`** — Execute legs with automated Developer + Reviewer agents

> "Start with `/init-project` on the project you just registered."

### 5. Offer Next Step

> "Want to run `/init-project` for {project-slug} now?"

If yes, invoke the `/init-project` skill for the registered project.

## Guidelines

### Keep It Quick

This is onboarding, not the main work. Get the registry set up and move on.

### Don't Over-Explain

The user will learn the methodology by using it. Give the orientation but don't lecture.

### Validate Paths

When the user provides a filesystem path, verify it exists before adding to the registry.
