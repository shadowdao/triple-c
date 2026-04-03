# Mission Control Setup Instructions

Reference document for adding Flight Control methodology to any project.

## How Triple-C Installs Mission Control

When Mission Control is enabled for a project in Triple-C:

1. **Bundled files install**: The Mission Control files bundled with Triple-C are copied to `/home/claude/mission-control/` (persisted in the config volume)
2. **Skills install**: All skills from the bundled `.claude/skills/` are copied to `~/.claude/skills/` so Claude Code discovers them automatically as `/slash-commands`
3. **Workspace symlink**: `/workspace/mission-control/` symlinks to the installed copy for methodology doc access
4. **Global instructions**: Mission Control usage instructions are injected into `~/.claude/CLAUDE.md`

This happens automatically on every container start, so skill updates from new Triple-C releases are picked up on restart.

## Two pieces are needed per project:

1. **Global CLAUDE.md** — Handled automatically by Triple-C when Mission Control is enabled
2. **Project CLAUDE.md** — Add the Flight Operations section to each project's `CLAUDE.md`

Then run `/init-project` to create the `.flightops/` directory.

---

## 1. Global CLAUDE.md Instructions

These are **automatically injected by Triple-C** when Mission Control is enabled. For reference, the injected content is:

```markdown
## Mission Control

The `/workspace/mission-control/` directory contains **Flight Control** — an AI-first development methodology for structured project management. Use it for all project work.

### How It Works

- **Mission Control is a tool, not a project.** It provides skills and methodology for managing other projects.
- All Flight Control skills are installed as personal skills in `~/.claude/skills/` and are automatically available as `/slash-commands`
- The methodology docs and project registry live in `/workspace/mission-control/`

### When to Use

When working on any project that has a `.flightops/` directory, follow the Flight Control methodology:
1. Read the project's `.flightops/ARTIFACTS.md` to understand artifact storage
2. Read `.flightops/FLIGHT_OPERATIONS.md` for the implementation workflow
3. Use Mission Control skills for planning and execution

### Available Skills

| Skill | When to Use |
|-------|-------------|
| `/init-project` | Setting up a new project for Flight Control |
| `/mission` | Defining new work outcomes (days-to-weeks scope) |
| `/flight` | Creating technical specs from missions (hours-to-days scope) |
| `/leg` | Generating implementation steps from flights (minutes-to-hours scope) |
| `/agentic-workflow` | Executing legs with multi-agent workflow (implement, review, commit) |
| `/flight-debrief` | Post-flight analysis after a flight lands |
| `/mission-debrief` | Post-mission retrospective after completion |
| `/daily-briefing` | Cross-project status report |

### Key Rules

- **Planning skills produce artifacts only** — never modify source code directly
- **Phase gates require human confirmation** — missions before flights, flights before legs
- **Legs are immutable once in-flight** — create new ones instead of modifying
- **`/agentic-workflow` orchestrates implementation** — it spawns separate Developer and Reviewer agents
- **Artifacts live in the target project** — not in mission-control
```

---

## 2. Project CLAUDE.md Instructions

Add this section to each project's `CLAUDE.md`:

```markdown
## Flight Operations

This project uses Flight Control (bundled with Triple-C) for structured development.

**Before any mission/flight/leg work, read these files in order:**
1. `.flightops/README.md` — What the flightops directory contains
2. `.flightops/FLIGHT_OPERATIONS.md` — **The workflow you MUST follow**
3. `.flightops/ARTIFACTS.md` — Where all artifacts are stored
4. `.flightops/agent-crews/` — Project crew definitions for each phase (read the relevant crew file)
```

---

## 3. Initialize the Project

After adding the CLAUDE.md sections, run `/init-project` from mission-control to:

1. Create the `.flightops/` directory with methodology references
2. Configure the artifact system (files or Jira)
3. Set up agent crew definitions
4. Register the project in `/workspace/mission-control/projects.md`

---

## Quick Checklist for New Projects

- [ ] Enable Mission Control for the project in Triple-C (auto-installs skills to `~/.claude/skills/`)
- [ ] Add Flight Operations section to the project's `CLAUDE.md`
- [ ] Run `/init-project` from mission-control
- [ ] Add the project to `/workspace/mission-control/projects.md`
- [ ] Add `.flightops/` to the project's `.gitignore` (if artifacts should not be committed) or commit it (if they should)
