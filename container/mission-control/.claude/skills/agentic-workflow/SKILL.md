---
name: agentic-workflow
description: Active orchestrator for multi-agent flight execution. Drives the full leg cycle (design, implement, review, commit) using three separate Claude instances.
---

# Agentic Workflow

Orchestrate multi-agent flight execution. You drive the full leg cycle — designing legs, spawning Developer and Reviewer agents, and managing git workflow — for a target project's flight.

## Prerequisites

- Project must be initialized with `/init-project` (`.flightops/ARTIFACTS.md` must exist)
- A mission must exist and be `active`
- A flight must exist and be `ready` or `in-flight`

## Invocation

```
/agentic-workflow flight {number} for {project-slug} mission {number}
```

Example: `/agentic-workflow flight 03 for epipen mission 04`

## Phase 1: Context Loading

1. **Read `projects.md`** to find the target project's path
2. **Read `{target-project}/.flightops/ARTIFACTS.md`** for artifact locations
3. **Read `{target-project}/.flightops/agent-crews/leg-execution.md`** for project crew definitions, interaction protocol, and prompts (fall back to defaults at `.claude/skills/init-project/defaults/agent-crews/leg-execution.md`)
   - **Validate structure**: The phase file MUST contain `## Crew`, `## Interaction Protocol`, and `## Prompts` sections. Each prompt subsection MUST have a fenced code block.
   - **If the file exists but is malformed**: STOP. Tell the user: "Phase file `leg-execution.md` is missing required sections. Either fix it manually or re-run `/init-project` to reset to defaults." Do NOT improvise missing prompts — halt and get the file fixed.
4. **Read the mission artifact** — outcomes, success criteria, constraints
5. **Read the flight artifact** — objective, design decisions, leg list
6. **Read the flight log** — ground truth from prior execution
7. **Count total legs** from the flight spec — track progress throughout
8. **Determine starting point** — which leg is next based on flight log and leg statuses
9. **Read git strategy** from `{target-project}/.flightops/ARTIFACTS.md` `## Git Workflow` section. Default to `branch` if the section is absent.
10. **Set `{working-directory}`** — `branch`: the target project root; `worktree`: the worktree path (see Git Workflow section below)

**Mark flight as in-flight**: After loading the flight artifact, if the flight status is `ready`, update it to `in-flight` before proceeding. If already `in-flight`, leave it as-is.

If resuming a flight already in progress, verify state consistency:
- Flight log entries must match leg statuses
- If discrepancies exist, remediate before proceeding

## Phase 2: Leg Cycle

Repeat for each leg in the flight.

### 2a: Leg Design

1. **Design the leg** using the `/leg` skill (if the Skill tool is unavailable, read `.claude/skills/leg/SKILL.md` and follow the workflow directly)
   - Read the flight spec, flight log, and relevant source code
   - Create the leg artifact with acceptance criteria
2. **Spawn a Developer agent for design review** (Task tool, `subagent_type: "general-purpose"`)
   - Working directory: `{working-directory}`
   - Provide the "Review Leg Design" prompt from the leg-execution phase file's Prompts section
   - The Developer reads the leg artifact and cross-references against actual codebase state
   - The Developer provides a structured assessment: approve, approve with changes, or needs rework
3. **Incorporate feedback** — update the leg artifact to address any issues raised
   - High-severity issues: must fix before proceeding
   - Medium-severity issues: fix unless there's a clear reason not to
   - Low-severity issues and suggestions: apply at discretion
4. **Re-review if substantive changes were made** — spawn another Developer for a second pass
   - Skip if only minor/cosmetic fixes were applied
   - If the second review raises new high-severity issues, fix and re-review once more
   - **Max 2 design review cycles** — if issues persist after 2 rounds, escalate to human
5. **Update leg status** to `ready`
6. **Signal `[HANDOFF:review-needed]`** when the leg design is finalized

### 2b: Leg Implementation

**NEVER implement code directly.** Spawn a Developer agent via the Task tool.

**Interactive/UAT legs**: If the leg is a UAT, alignment, or other interactive leg (identified by slug like `uat-*`, `alignment-*`, or explicit marking in the flight spec), do NOT spawn agents to execute it autonomously. The human performs verification — the Flight Director guides them through it:
1. **Design the leg** normally (2a), but keep it lightweight — the acceptance criteria are verification steps, not implementation tasks
2. **Skip the autonomous implementation cycle** (no Developer/Reviewer agents)
3. **Guide the human through verification steps one at a time** — present a single step, wait for the human to perform it and report results, then proceed to the next step
4. **Fix issues inline** — if the human reports a failure, diagnose and fix it (spawning a Developer agent if code changes are needed), then re-verify that step before moving on
5. **Commit when all steps pass** — spawn a Developer agent to update artifacts and commit

**Standard (autonomous) legs**: Follow the Developer/Reviewer cycle below.

1. **Spawn a Developer agent** (Task tool, `subagent_type: "general-purpose"`)
   - Working directory: `{working-directory}`
   - Provide the "Implement" prompt from the leg-execution phase file's Prompts section
   - The Developer updates leg status to `in-flight`, implements to acceptance criteria
   - When done, the Developer updates leg status to `landed`, updates flight log, and signals `[HANDOFF:review-needed]` — do NOT let it commit
2. **Spawn a Reviewer agent** (Task tool, `subagent_type: "general-purpose"`)
   - Working directory: `{working-directory}`
   - Provide the "Review" prompt from the leg-execution phase file's Prompts section
   - The Reviewer evaluates ALL uncommitted changes against acceptance criteria and code quality
   - The Reviewer signals `[HANDOFF:confirmed]` or lists issues with severity
3. **If issues found**, spawn a new Developer agent to fix them
   - Provide the "Fix Review Issues" prompt from the leg-execution phase file with the Reviewer's feedback
   - Loop review/fix until the Reviewer confirms
4. **Spawn the Developer agent to commit** after review passes
   - Provide the "Commit" prompt from the leg-execution phase file's Prompts section
   - The commit must include code changes, updated flight log, and leg status updated to `completed`

### 2c: Leg Transition

After `[COMPLETE:leg]` (all git/PR operations run from `{working-directory}`):
1. Increment `legs_completed`
2. **Manage PR**:
   - **First leg**: Open a draft PR with the leg checklist in the body (see PR Body Format below), then check off the completed leg
   - **Subsequent legs**: Use `gh pr edit --body` to check off the newly completed leg in the existing PR body
3. If more legs remain → return to 2a
4. If all legs complete → proceed to Phase 3

## Phase 3: Flight Completion

1. **Verify all legs** show `completed` status
2. **Verify flight log** has entries for all legs
3. **Verify documentation** — check that CLAUDE.md, README, and other project docs reflect any new commands, endpoints, configuration, or APIs introduced during the flight. If not, spawn a Developer agent to update them.
4. **Update flight status** to `landed`
5. **Check off flight** in mission artifact
6. **Clean up worktree** (worktree strategy only) — run `git worktree remove` after the PR is marked ready for review
7. **Signal `[COMPLETE:flight]`**

The flight debrief is a separate step run via `/flight-debrief` after the flight lands. The debrief transitions the flight to `completed`.

## Architecture

The Flight Director (you) orchestrates according to this skill. Project crew composition, roles, models, and prompts are defined in `{target-project}/.flightops/agent-crews/leg-execution.md`.

**Separation is mandatory.** Project crew agents run in the target project and load its CLAUDE.md and conventions. The Reviewer has no knowledge of the Developer's reasoning — only the resulting changes. This provides objective review.

**Model selection:** Follow the model preferences in the phase file. MC may use Opus for complex planning. Never use Opus for the Reviewer.

## Handoff Signals

Signals are part of the Flight Control methodology and are NOT configurable per-project. All crew agents must use these exact signals:

| Signal | Emitted By | Meaning |
|--------|-----------|---------|
| `[HANDOFF:review-needed]` | Developer | Code/artifact ready for review |
| `[HANDOFF:confirmed]` | Reviewer | Review passed |
| `[BLOCKED:reason]` | Any crew agent | Cannot proceed, needs resolution |
| `[COMPLETE:leg]` | Developer | Leg finished and committed |
| `[COMPLETE:flight]` | Flight Director | Flight landed |

## Flight Director Decision Log

The Flight Director must maintain transparency about its own decisions. After each major orchestration step, log what happened and why in the flight log under a `### Flight Director Notes` subsection:

1. **Phase file loading** — Record which phase file was loaded (project or default fallback) and what crew was extracted
2. **Agent spawning** — Record which agent was spawned, with what prompt, and what model
3. **Review cycle decisions** — When incorporating feedback, note what was accepted/rejected and why
4. **Escalation decisions** — When choosing between "fix and re-review" vs "escalate to human," note the reasoning
5. **Signal interpretation** — When a crew agent's output is ambiguous, note how it was interpreted

This is not a separate file — it goes in the flight log alongside leg entries. The goal is that anyone reviewing the flight log can understand not just what the crew did, but why the Flight Director made the orchestration choices it did.

## Git Workflow

### Strategy Selection

Read the `## Git Workflow` section from `{target-project}/.flightops/ARTIFACTS.md`. The `Strategy` property determines which workflow to use. If the section is absent, default to `branch`.

### Shared Elements

Both strategies use the same branch naming, commit format, PR lifecycle, and PR body format.

**Branch naming**: `flight/{number}-{slug}`

**Commit message format:**
```
leg/{number}: {description}

Flight: {flight-number}
Mission: {mission-number}
```

**PR lifecycle:**

| Event | Action |
|-------|--------|
| First leg complete | Open draft PR with leg checklist in body |
| Each leg complete | Commit code + artifacts, update PR checklist |
| Flight landed | Mark PR ready for review |

**PR body format:**

```markdown
## {Flight Title}

{Flight objective — one paragraph}

**Mission**: {Mission Title}

## Legs

- [ ] `{leg-slug}` — {brief description}
- [ ] `{leg-slug}` — {brief description}
```

### Strategy: Branch

The default single-checkout workflow. One flight at a time per working copy.

| Step | Command |
|------|---------|
| Flight start | `git checkout -b flight/{number}-{slug}` |
| Set `{working-directory}` | Target project root |
| Agents work in | Project root |
| Flight landed | PR marked ready for review |

### Strategy: Worktree

Worktree isolation enables parallel flights on a single repo clone.

| Step | Command |
|------|---------|
| Flight start | `git worktree add .worktrees/flight-{number}-{slug} -b flight/{number}-{slug}` |
| Set `{working-directory}` | `.worktrees/flight-{number}-{slug}` |
| Orchestrator stays on | Main branch (does not checkout the flight branch) |
| Agents work in | Worktree path |
| Flight landed | PR marked ready for review, then `git worktree remove .worktrees/flight-{number}-{slug}` |

**Note:** The `.worktrees/` directory must be in `.gitignore` when using this strategy.

## Error Handling

| Situation | Action |
|-----------|--------|
| Developer agent fails mid-leg | Spawn new Developer with context of what failed |
| Design review loops > 2 times | Escalate to human with unresolved design issues |
| Code review loops > 3 times | Escalate to human |
| Leg marked aborted | Escalate to human with abort details |
| Artifact discrepancy | Remediate before proceeding |
| Off the rails | Roll back to last leg commit, escalate |
| Stale worktree (worktree strategy) | Run `git worktree prune`, recreate if needed |
| Agent hangs on tests | Kill the agent, spawn new Developer to isolate and fix hanging tests |
