---
name: daily-briefing
description: Cross-project status report with health assessment, stale work detection, and methodology insights. Use for a quick overview of all managed projects.
---

# Daily Briefing

Generate a comprehensive status report across managed projects with health assessments, staleness detection, and cross-project methodology insights.

## Prerequisites

- `projects.md` must exist (run `/init-mission-control` first)

## Output

Daily briefings are saved to the `daily-briefings/` directory in the mission-control repository root, named by date: `daily-briefings/YYYY-MM-DD.md`. This directory is gitignored — briefings are local-only, ephemeral documents.

If a briefing already exists for today, append a sequence number: `YYYY-MM-DD-2.md`, `YYYY-MM-DD-3.md`, etc.

## Workflow

### Phase 1: Load Projects Registry

1. **Read `projects.md`** to get the full list of managed projects
2. Extract each project's slug, description, path, and status

### Phase 2: Project Selection Interview

Present the user with a **checkbox list** (AskUserQuestion with `multiSelect: true`) of all registered projects so they can select which ones to include in the briefing.

**AskUserQuestion limits each question to 4 options.** Split projects across multiple questions in groups of 3, reserving the 4th option as "None from this group" (description: "Skip these projects") so the user can opt out of an entire group without blocking submission. Send all questions in a single AskUserQuestion call.

Format each project option as:
- **Label**: Project slug
- **Description**: Short project description from the registry

### Phase 3: Project Scanning

For each selected project, gather data by spawning **parallel Explore agents** (Task tool, `subagent_type: "Explore"`) — one per project. Each agent should:

1. **Check initialization status**
   - Check if `{project-path}/.flightops/ARTIFACTS.md` exists
   - If missing, note the project as "not initialized" and skip artifact scanning

2. **Read artifact configuration**
   - Read `{project-path}/.flightops/ARTIFACTS.md` for directory structure and naming conventions

3. **Discover all artifacts**
   - For filesystem-based projects, scan `{project-path}/missions/` for mission directories
   - For each mission, scan for flights; for each flight, scan for legs
   - Read all discovered artifacts and capture:
     - **Status fields** from each mission, flight, and leg
     - **Titles and objectives**
     - **Checklist completion** (count checked vs unchecked items)

4. **Read debriefs**
   - Read any flight debriefs and mission debriefs found
   - Extract key learnings, recommendations, and action items

5. **Check git activity**
   - Run `git log --oneline --since="7 days ago" -20` in the project directory
   - Capture recent commit activity as a proxy for momentum

6. **Return structured findings**
   The agent should return a structured summary including:
   - Project initialization status
   - List of all missions with status
   - List of all flights with status
   - List of all legs with status
   - Debrief summaries (key learnings and recommendations)
   - Recent git activity summary
   - Any anomalies (e.g., in-flight legs with no recent commits)

### Phase 4: Health Analysis

For each scanned project, assess health across these dimensions:

#### Activity Status
- **Active**: Has in-flight work AND recent commits
- **Stalled**: Has in-flight work but NO recent commits (7+ days)
- **Idle**: No in-flight work
- **Fresh**: Recently completed work (within 7 days)

#### Staleness Detection
Flag artifacts that appear stale or abandoned:
- **Missions** with status `active` or `planning` but no flight activity in 14+ days
- **Flights** with status `in-flight` or `planning` but no leg progress in 7+ days
- **Legs** with status `in-flight` or `planning` but no recent commits in 7+ days
- **Open questions** that remain unresolved across any active artifacts

#### Completion Assessment
- Missions nearing completion (most success criteria checked)
- Flights with all legs completed but not yet marked `landed`
- Orphaned artifacts (legs without flights, flights without missions)

### Phase 5: Cross-Project Insights

Analyze all debriefs (flight and mission) across selected projects to extract:

#### Common Patterns
- Recurring recommendations across projects
- Shared technical or process challenges
- Patterns in what goes well vs what struggles

#### Mission Control Methodology Improvements
- Feedback about the mission/flight/leg hierarchy itself
- Skill effectiveness observations (were leg specs clear enough? were flight plans accurate?)
- Suggestions for new skills, templates, or workflow changes

#### Project-Specific Recommendations
- Per-project action items drawn from debrief insights
- Suggested next steps based on current state

### Phase 6: Generate Briefing

Create the daily briefing file at `daily-briefings/YYYY-MM-DD.md`. Ensure the `daily-briefings/` directory exists first.

**Format:**

```markdown
# Daily Briefing — {YYYY-MM-DD}

## Executive Summary
{2-3 sentence overview of portfolio health — how many projects active, key highlights, top concerns}

---

## Project Reports

### {Project Slug}

**Health**: {Active | Stalled | Idle | Fresh} {optional: brief qualifier}
**Recent Activity**: {X commits in last 7 days | No recent commits}

#### Current State
| Level | Active | Completed | Stale | Total |
|-------|--------|-----------|-------|-------|
| Missions | {n} | {n} | {n} | {n} |
| Flights | {n} | {n} | {n} | {n} |
| Legs | {n} | {n} | {n} | {n} |

#### In Progress
- **Mission**: {title} — {status summary}
  - **Flight**: {title} — {status}, {X/Y legs complete}
    - {Current/next leg and its status}

#### Staleness Alerts
- {Description of stale artifact and how long it's been inactive}

#### Recommendations
- {Actionable recommendation based on current state and debrief insights}

---

{Repeat for each selected project}

---

## Not Initialized
{List any selected projects that lack `.flightops/` — suggest running `/init-project`}

---

## Cross-Project Insights

### Common Patterns
{Themes observed across multiple project debriefs}

### Methodology Observations
{Feedback about Flight Control itself — what's working, what could improve}

### Recommended Actions
1. {Highest-priority cross-project action}
2. {Second priority}
3. {Third priority}
```

### Phase 7: Present Summary

After writing the briefing file, present a **concise verbal summary** to the user:

1. The file path where the full briefing was saved
2. Portfolio-level health (X active, Y stalled, Z idle)
3. Top 3 items needing attention (stale work, approaching completions, blockers)
4. Any cross-project methodology insights worth highlighting

Keep the verbal summary short — the detailed report is in the file.

## Guidelines

### Read-Only
This skill only reads project artifacts and git history. It **never** modifies any project files, artifacts, or source code.

### Parallel Scanning
Spawn project scanning agents in parallel for efficiency. Don't scan projects sequentially.

### Graceful Degradation
- Projects without `.flightops/` get noted but don't block the report
- Projects with no missions directory get reported as "no Flight Control artifacts"
- Missing or malformed artifacts get flagged, not crashed on

### Honest Assessment
Report what you find. Don't sugarcoat stale projects or inflate activity. The briefing is for situational awareness — accuracy matters more than optimism.

### Brevity in Verbal Summary
The written briefing is the detailed artifact. The verbal summary should be 5-10 lines max — just the highlights and the file path.
