# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Invocation Context

You may be invoked by:
- **A human** — Interactive session, ask questions freely
- **An LLM orchestrator** — Run `/agentic-workflow` to drive multi-agent flight execution

When orchestrated, you are the **Flight Director** — responsible for driving execution, coordinating agents, and making go/no-go decisions. Emit signals like `[HANDOFF:review-needed]` and `[COMPLETE:leg]` at appropriate points. The orchestrator monitors your output for these markers.

**When a human says a leg is ready to implement**, invoke `/agentic-workflow`. Do not read the leg spec, do not plan execution steps, do not execute commands directly. You become the orchestrator by loading the skill.

### Loading Skills in Non-Interactive Contexts

**The Skill tool is ONLY available in interactive human sessions.** If you are a spawned agent, running via `claude -p`, or inside a container/SDK — you do NOT have the Skill tool. Do not attempt to call it.

To execute a skill, read its SKILL.md file directly and follow the workflow:

```
Read .claude/skills/{skill-name}/SKILL.md and execute the workflow described there.
```

**All Flight Control skills** (listed in the table below) **live in this repository** (mission-control), under `.claude/skills/`. The Flight Director runs from the mission-control directory, so relative `.claude/skills/` paths in skill docs resolve here. Target projects may have their own unrelated skills in their own `.claude/skills/` directories — those are separate.

## First-Contact Check

If `projects.md` does not exist in this repository, suggest running `/init-mission-control` to set up the projects registry before proceeding with any other skills.

## Project Overview

Flight Control is an AI-first software development lifecycle methodology using aviation metaphors. It organizes work into three hierarchical levels:

- **Missions** (human-optimized) — Define outcomes in human terms, days-to-weeks scope
- **Flights** (balanced) — Technical specifications with pre/in/post-flight checklists, hours-to-days scope
- **Legs** (AI-optimized) — Structured implementation steps with explicit acceptance criteria, minutes-to-hours scope

This repository contains the methodology documentation and Claude Code skills for interactive planning.

## Claude Code Skills

Eleven skills automate the planning, execution, debrief, and oversight workflow:

| Skill | Purpose |
|-------|---------|
| `/init-mission-control` | Onboard to Mission Control (set up `projects.md` registry) |
| `/init-project` | Initialize a project for Flight Control (creates `.flightops/` directory) |
| `/mission` | Create outcome-driven missions through research and interview |
| `/flight` | Create technical flight specs from missions |
| `/leg` | Generate implementation guidance for LLM execution |
| `/agentic-workflow` | Drive multi-agent flight execution (design, implement, review, commit) |
| `/flight-debrief` | Post-flight analysis for continuous improvement |
| `/mission-debrief` | Post-mission retrospective for outcomes assessment |
| `/routine-maintenance` | Post-mission codebase health assessment and maintenance recommendation |
| `/preflight-check` | Verify all projects have current methodology files and crew definitions |
| `/daily-briefing` | Cross-project status report with health assessment and methodology insights |

Run `/init-project` before using the other skills on a new project to create the flight operations reference directory and configure the artifact system.

**Artifact Systems:** Each project defines how artifacts are stored in `.flightops/ARTIFACTS.md`. Skills read this configuration and adapt their output accordingly.

**IMPORTANT: Planning skills produce documentation only.** `/init-project`, `/mission`, `/flight`, `/leg`, `/flight-debrief`, `/mission-debrief`, and `/routine-maintenance` must:
- **NEVER implement code changes** — only create/update artifacts
- **NEVER modify source files** in the target project (no `.rs`, `.ts`, `.tsx`, `.json`, etc.)

`/agentic-workflow` orchestrates implementation by spawning separate agents that execute code changes in the target project. The orchestrator itself never modifies source files directly.

> **Phase gates require confirmation.** Missions must be fully agreed before designing
> flights. Flights must be fully agreed before designing legs. Never skip ahead — get
> explicit user confirmation at each transition.

## Projects Registry

The `projects.md` file in this repository catalogs all active projects on this device. When using skills:

1. **Read `projects.md` first** to find the target project's path, remote, and description
2. **Read `.flightops/ARTIFACTS.md`** in the target project to determine artifact locations
3. **Create all artifacts in the target project** — not in mission-control

The registry provides:
- Project slug and description
- Filesystem path (e.g., `~/projects/my-app`)
- Git remote
- Optional stack and status information

## Lifecycle States

- **Missions**: `planning` → `active` → `completed` (or `aborted`)
- **Flights**: `planning` → `ready` → `in-flight` → `landed` → `completed` (or `aborted`)
- **Legs**: `planning` → `ready` → `in-flight` → `landed` → `completed` (or `aborted`)

## Public Repository

This is a public repository. Keep all committed content anonymized:

- **No personal paths** — Use generic examples like `~/projects/my-app`, not actual home directories
- **No usernames** — Use placeholders like `username` in examples
- **No project-specific details** — Keep examples generic
- `projects.md` is gitignored for this reason — it contains local paths and is not committed
