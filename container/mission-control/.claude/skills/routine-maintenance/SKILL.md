---
name: routine-maintenance
description: Post-mission codebase health assessment. Run after `/mission-debrief` to verify the codebase is flight-ready or scaffold a maintenance mission. Per-flight findings instead roll into the next flight or accumulate into an end-of-mission maintenance flight — not into this skill.
---

# Routine Maintenance

Perform an exhaustive, aviation-style codebase inspection after a mission completes. Produces a findings report and optionally scaffolds a maintenance mission for significant issues.

## Prerequisites

- Project must be initialized with `/init-project` (`.flightops/ARTIFACTS.md` must exist)

## Workflow

### Phase 1: Context Loading

1. **Identify the target project**
   - Read `projects.md` to find the project's path

2. **Verify project is initialized**
   - Check if `{target-project}/.flightops/ARTIFACTS.md` exists
   - **If missing**: STOP and tell the user to run `/init-project` first
   - Do not proceed without the artifact configuration

3. **Read the artifact configuration**
   - Read `{target-project}/.flightops/ARTIFACTS.md` for artifact locations and formats

4. **Load prior maintenance reports (if any exist)**
   - Read previous reports in `maintenance/` to identify deferred findings from earlier cycles
   - Deferred findings are those documented in prior reports but not addressed by a maintenance mission
   - This ensures recurring issues are tracked across cycles rather than re-discovered as "new"

5. **Load mission and debrief documentation**
   - Read the most recently completed mission for outcome, success criteria, and known issues
   - Read its mission debrief for lessons learned and action items
   - Read its flight debriefs for per-flight technical debt and recommendations
   - This provides known-debt context so the inspection can distinguish new issues from acknowledged ones

6. **Identify project stack**
   - Read `README.md`, `CLAUDE.md`, and package files (`package.json`, `Cargo.toml`, `go.mod`, etc.)
   - Determine language, framework, test runner, linter, formatter, type checker, and dependency audit tool

### Phase 2: Scoping Interview

Two parts: category enablement and inspection focus.

#### Category Enablement

Categories 1–7 always apply to every project. Ask the user yes/no for optional categories:

> "Before I begin the inspection, a few quick questions:"
>
> 1. "Does this project have CI/CD pipelines?" → enables Category 8 + CI/CD Reviewer
> 2. "Does this project have deployments, databases, or environment-specific configs?" → enables Category 9
> 3. "Does this project have monitoring, metrics, or observability tooling?" → enables Category 10
> 4. "Does this project have user-facing UI (web pages, components, forms)?" → enables Accessibility Reviewer

#### Inspection Focus

> 5. "Where should the inspection focus?"
>    - **Recent changes** — "Since when? (last mission, last flight, specific date, N days)"
>    - **Specific areas** — "Which modules, directories, or concerns?"
>    - **Full codebase** — "Everything"
>
> "Any specific areas of concern for this project?"

Record all responses. The focus selection and areas of concern drive the delegation plan in Phase 3.

### Phase 3: Delegation Planning

Before spawning any agents, the Flight Director creates an explicit plan for how to partition work across sub-agents. Each sub-agent has its own context window — the delegation plan ensures no single agent is overwhelmed by project scale.

#### 3a: Assess Project Size

Run a quick structural assessment:

1. **Count source files** — `find {target-project} -name "*.{ext}" -not -path "*/node_modules/*" -not -path "*/.git/*" | wc -l` (use appropriate extensions for the stack)
2. **Identify module boundaries** — check for workspace configs (`Cargo.toml` workspaces, `package.json` workspaces, `go.work`), top-level directories with independent configs, or monorepo structure
3. **Classify project size**:
   - **Small**: < 100 source files
   - **Medium**: 100–500 source files
   - **Large**: > 500 source files

#### 3b: Determine Scope

Based on the interview focus selection:

- **Recent changes**: Run `git log` / `git diff` to identify changed files since the user's specified cutoff. Map changed files to their containing modules. The inspection scope is the changed files plus their immediate dependencies (imports, shared types, test files).
- **Specific areas**: Scope is the user-specified modules/directories. Identify their boundaries and dependencies.
- **Full codebase**: Scope is everything. For large projects, this triggers module partitioning (see 3c).

#### 3c: Build the Delegation Plan

The plan determines how many agents to spawn, what each one covers, and in what order. Present it to the user for approval before executing.

**Concurrency constraint**: Claude Code supports a default maximum of **4 concurrent sub-agents**. Additional agents are queued automatically and start as running agents complete. The delegation plan must account for this by organizing agents into **waves** of up to 4.

**Sub-agent context constraint**: Each sub-agent starts with a fresh context window containing only its prompt. All sub-agent output returns to the Flight Director's context. Verbose results from multiple agents accumulate and can exhaust the Flight Director's context — this makes output discipline a hard constraint, not a suggestion.

**Delegation rules by scope and size:**

| Scope | Small/Medium Project | Large Project |
|-------|---------------------|---------------|
| **Recent changes** | One agent per role, scoped to changed files | Same — changed files are naturally bounded |
| **Specific areas** | One agent per role, scoped to specified areas | Same — user has already bounded the scope |
| **Full codebase** | One agent per role, full project | **Partition**: see below |

**Full codebase on large projects — partitioning strategy:**

The Inspector is the most context-hungry role. Partition it:

1. **Tool-pass Inspector**: One agent that runs all automated tools (linters, type checkers, test suite, audit commands) across the full project. Tool output is structured and concise — this agent reads command output, not source code.
2. **Module Inspectors**: One agent per identified module boundary. Each performs manual code review (dead code, patterns, TODOs, duplication) only within its module. If no clear module boundaries exist, partition by top-level source directories.

Specialist reviewers (Security, CI/CD, Accessibility) are naturally scoped by their domain and generally do not need partitioning. If a specialist reviewer's scope is too broad (e.g., security review of a 1000-file project), scope it to: files flagged by the tool-pass Inspector + files in security-critical paths (auth, API handlers, data access).

**Wave planning**: Assign agents to waves of up to 4 concurrent agents. Prioritize by dependency:
- **Wave 1**: Tool-pass Inspector + specialist reviewers (Security, CI/CD, Accessibility) — up to 4 agents. The tool pass runs automated commands; specialists do independent manual review. These have no dependencies on each other.
- **Wave 2** (if partitioned): Module Inspectors — these can optionally use tool-pass findings to focus their manual review, so they benefit from running after Wave 1. Up to 4 module agents per wave.

For small/medium projects or scoped inspections, everything fits in a single wave.

**The delegation plan structure:**

> **Delegation Plan**
>
> Scope: {recent changes since X | specific areas: X, Y | full codebase}
> Project size: {small | medium | large} (~{N} source files)
> Modules: {list of identified modules, or "flat project"}
>
> **Wave 1** (concurrent):
> | # | Agent | Role | Scope | Strategy |
> |---|-------|------|-------|----------|
> | 1 | Inspector (tools) | Run automated tools | Full project | Commands only, no code reads |
> | 2 | Security Reviewer | Auth & injection review | src/auth/, src/api/handlers/ | Manual code path analysis |
> | 3 | CI/CD Reviewer | Pipeline review | .github/workflows/ | Config review |
> | 4 | Accessibility Reviewer | WCAG 2.1 AA review | src/components/ | UI review |
>
> **Wave 2** (after Wave 1 completes):
> | # | Agent | Role | Scope | Strategy |
> |---|-------|------|-------|----------|
> | 5 | Inspector (module A) | Manual code review | src/api/ | Categories 4-6 |
> | 6 | Inspector (module B) | Manual code review | src/core/ | Categories 4-6 |
>
> "Here's how I plan to partition the inspection. Want to adjust anything?"

For small/medium projects or scoped inspections, the plan is straightforward (one wave, one agent per role) and can be presented briefly. Only expand the full table for large/full-codebase inspections.

#### 3d: Refinement

This delegation strategy is expected to evolve. After each maintenance run, note in the flight log or maintenance report whether the partitioning was effective:
- Did any agent hit context limits or return shallow results?
- Were module boundaries appropriate?
- Should specialists have been scoped differently?

These observations inform future delegation plans for the same project.

### Phase 4: Specialist Review

Read `{target-project}/.flightops/agent-crews/routine-maintenance.md` for crew definitions and prompts (fall back to defaults at `.claude/skills/init-project/defaults/agent-crews/routine-maintenance.md`).

**Validate structure**: The crew file MUST contain `## Crew`, `## Interaction Protocol`, and `## Prompts` sections with fenced code blocks. If the file exists but is malformed, STOP and tell the user: "Crew file `routine-maintenance.md` is missing required sections. Either fix it manually or re-run `/init-project` to reset to defaults."

Execute the delegation plan from Phase 3, wave by wave. Spawn all agents within a wave in parallel (up to 4 concurrent). Wait for a wave to complete before starting the next. Every reviewer is strictly **read-only** — they may run test suites, linters, type checkers, and audit commands, but must NEVER modify source files, configuration, or dependencies.

Each agent receives:
- Its assigned scope from the delegation plan
- The relevant prompt from the crew file
- The output discipline rules (see Guidelines)

**Context management**: As each agent completes, summarize its findings before storing them for downstream use. Do not accumulate full raw output from every agent — the Flight Director's context is finite.

#### Inspector

1. **Spawn Inspector agent(s)** per the delegation plan (Agent tool, `subagent_type: "general-purpose"`)
   - Provide the "Inspect Codebase" prompt from the crew file's Prompts section
   - Include: applicable category list, project stack info, known debt from debriefs, user's areas of concern, and **scope assignment from the delegation plan**
   - For partitioned inspections: each Inspector agent receives its module/area scope and only the categories relevant to its assignment
   - In addition to the crew prompt, the Flight Director directly instructs the Inspector to: scan recent flight debriefs for any test metrics observations (timing, failures, skipped tests, flakes) and look for trends across them — rising suite times, recurring flakes, growing skip lists, persistent failures. Surface concrete optimization recommendations as Category 2 findings (e.g. parallelization, mocking, fixture hoisting, slow-test extraction, runner config), each tied to evidence from the trend.
   - The Inspector performs broad automated checks and returns structured findings per category

#### Security Reviewer

1. **Spawn a Security Reviewer agent** (Agent tool, `subagent_type: "general-purpose"`)
   - Provide the "Review Security" prompt from the crew file's Prompts section
   - Include: project stack info, known security debt from debriefs, and **scope assignment from the delegation plan**
   - The Security Reviewer performs focused manual analysis of auth flows, injection surfaces, secrets handling, and data exposure — deeper than the Inspector's Category 1 automated checks

#### CI/CD Reviewer (if enabled)

1. **Spawn a CI/CD Reviewer agent** (Agent tool, `subagent_type: "general-purpose"`)
   - Provide the "Review CI/CD" prompt from the crew file's Prompts section
   - Include: project stack info, known CI/CD debt from debriefs
   - The CI/CD Reviewer evaluates pipeline definitions, build security, deployment safeguards, and environment consistency

#### Accessibility Reviewer (if enabled)

1. **Spawn an Accessibility Reviewer agent** (Agent tool, `subagent_type: "general-purpose"`)
   - Provide the "Review Accessibility" prompt from the crew file's Prompts section
   - The Accessibility Reviewer evaluates against WCAG 2.1 AA standards, semantic HTML, keyboard navigation, screen reader compatibility, and color contrast

#### Merge Partitioned Results

If the Inspector was partitioned into multiple agents, merge their findings into a single consolidated findings list before proceeding to Phase 5. De-duplicate findings that appear in multiple modules.

### Phase 5: Severity Assessment & Roundtable

#### 5a: Initial Assessment

1. **Spawn an Architect agent** (Opus) with all reviewer findings + debrief context (Agent tool, `subagent_type: "general-purpose"`)
   - Provide the "Assess Findings" prompt from the crew file's Prompts section
   - Include: all reviewer findings, known debt items (if available from debriefs)
   - The Architect assigns an initial severity to each finding and raises challenges or questions directed at specific reviewers where findings seem incorrect, overlapping, or missing context

#### 5b: Roundtable

The Architect and specialist reviewers hash out findings through structured discussion. The Flight Director mediates.

1. **Route Architect's challenges** to the relevant reviewers
   - For each reviewer with outstanding challenges, spawn them with the "Roundtable Rebuttal" prompt from the crew file
   - Include the Architect's specific challenges directed at that reviewer
   - Reviewers respond with evidence, rebuttals, or concurrence
2. **Collect responses** from all challenged reviewers
3. **Return to Architect** with roundtable responses
   - Spawn Architect with the "Roundtable Resolution" prompt from the crew file
   - Include all reviewer rebuttals and new evidence
   - Architect produces final assessment incorporating the discussion
4. **Max 2 roundtable cycles** — if consensus isn't reached after 2 rounds, present both perspectives to the human in Phase 6 and let them decide

#### Severity Scale

| Severity | Meaning |
|----------|---------|
| **Pass** | No issue found |
| **Advisory** | Minor issue, deferring is acceptable |
| **Action Required** | Should be addressed before next major work cycle |
| **Critical** | Blocks further work, immediate attention needed |

The Architect produces an overall assessment:
- **Flight Ready** — All findings are Pass or Advisory
- **Maintenance Required** — Any finding is Action Required or Critical

### Phase 6: Human Review and Scoping

Present findings grouped by severity (Critical first, then Action Required, Advisory, Pass):

> **Overall Assessment: {Flight Ready | Maintenance Required}**
>
> {Findings summary table}

Then ask:
1. "Do these findings match your sense of the codebase health?"
2. "Any findings to override or adjust severity?"

Apply any overrides the user requests.

#### Scope Selection

If the assessment is Maintenance Required, help the user choose a manageable scope rather than scaffolding everything. Present a recommended shortlist:

> **Recommended scope** (Critical items are always included):
>
> {Numbered list of Critical + top Action Required findings, capped at ~5-7 items}
>
> {Count} additional Action Required and {count} Advisory findings are documented in the report for a future cycle.
>
> "Want me to scaffold a maintenance mission for these items? You can add or remove findings from the list."

The goal is a mission that can land in a single focused session. All findings are captured in the report regardless — deferred items aren't lost, they'll surface again in the next maintenance cycle. This keeps maintenance approachable even when the backlog is large.

### Phase 7: Generate Maintenance Report

Create the maintenance report artifact at the location defined in `.flightops/ARTIFACTS.md` (typically `maintenance/YYYY-MM-DD.md`). If a report already exists for today's date, append a numeric suffix (e.g., `2026-03-26-2.md`).

**Report contents:**
- Title (date-based) and date
- Optional "Triggered by" link to the mission that prompted the inspection (if applicable)
- Inspection scope and delegation plan summary
- Overall assessment (Flight Ready / Maintenance Required)
- Categories inspected
- Executive summary
- Findings by category (each with severity, description, evidence, recommendation)
- Severity summary (counts per level)
- Known debt carried forward (from debriefs, acknowledged but not addressed)
- Delegation effectiveness notes (for refining future inspections)
- Recommendations

### Phase 8: Scaffold Maintenance Mission (conditional)

**Only if**: Overall assessment is Maintenance Required AND the user confirmed they want a maintenance mission. Only the findings the user selected in Phase 6 are scaffolded — deferred findings remain in the report for future cycles.

This phase produces the full artifact tree — mission, flights, and legs — so the maintenance work is ready for `/agentic-workflow` execution without running `/mission`, `/flight`, or `/leg` separately.

#### 8a. Mission

Scan existing `missions/` directories to determine the next sequence number `{NN}`. Create `missions/{NN}-maintenance/mission.md` using the standard mission format from `.flightops/ARTIFACTS.md`:
- **Status**: `planning`
- **Outcome**: "Resolve codebase health issues identified in maintenance report {YYYY-MM-DD}"
- **Context**: Link to the maintenance report in `maintenance/{YYYY-MM-DD}.md`
- **Success Criteria**: One criterion per selected finding
- **Flights**: List the flights from step 8b
- Populate all standard mission sections. Mark sections with no relevant content as "N/A" (e.g., Open Questions, Stakeholders).

#### 8b. Flights

Re-group the user's selected findings into flights. Use the Architect's recommended groupings as a starting point, but adjust for any findings the user removed or added during Phase 6 scoping. Typical groupings: one flight per category with actionable findings, or by technical area when findings from different categories affect the same subsystem.

Each flight gets its own directory with `flight.md` and `flight-log.md`. Use the standard formats from `.flightops/ARTIFACTS.md` with these maintenance-specific notes:

- **Status**: `ready` — maintenance flights skip the Pre-Flight phase (no open questions or design decisions to resolve for concrete fixes). Mark Pre-Flight Checklist items as N/A.
- **Mission**: Link back to the maintenance mission
- **Objective**: What this group of fixes accomplishes
- **Technical Approach**: Brief description of the fix strategy per finding
- **Legs**: List the legs from step 8c
- Populate all other standard flight sections. Mark sections with no relevant content as "N/A".

The `flight-log.md` is created empty (header only) — it will be populated during execution.

#### 8c. Legs

Each flight gets one leg per discrete fix. Create leg files using the standard format from `.flightops/ARTIFACTS.md` with these fields populated:

- **Status**: `ready`
- **Flight**: Link back to the flight
- **Objective**: Fix one specific finding (reference the finding number from the report)
- **Context**: Link to the maintenance report finding and the Architect's recommendation
- **Inputs/Outputs**: Files that exist before and after the fix
- **Acceptance Criteria**: The specific condition that resolves the finding — derived from the Architect's recommendation
- **Verification Steps**: How to confirm the fix (e.g., "run `npm audit` and confirm no high/critical vulnerabilities", "run `cargo clippy` with no warnings")
- **Implementation Guidance**: Concrete steps to resolve the finding, based on the Inspector's evidence and the Architect's recommendation
- **Files Affected**: List files identified in the Inspector's evidence
- Mark sections with no relevant content as "N/A" (e.g., Edge Cases for straightforward dependency updates).

Keep legs atomic — one finding, one fix. If a finding requires touching many files but is conceptually one change (e.g., "replace all `any` casts"), that's still one leg.

#### 8d. Update Report Backlink

After scaffolding, update the "Maintenance Mission" section at the bottom of the maintenance report (from Phase 7) with a link to the newly created mission.

## Guidelines

### Read-Only Inspection

This skill NEVER modifies source files, configuration, or dependencies in the target project. The Inspector runs checks and reports findings. The only files created are the maintenance report and optionally a full maintenance mission scaffold (mission, flights, and legs) — all are Flight Control artifacts.

### Output Discipline

This is a **hard constraint**, not a suggestion. All sub-agent output returns to the Flight Director's context window. With 4+ reviewers plus roundtable agents, verbose output will exhaust the Flight Director's context and break the workflow.

All reviewer agents must follow these rules:

1. **Summary-first**: Each finding includes a title, severity estimate, file path, and a one-line evidence summary
2. **Selective detail**: Include code excerpts or extended evidence only for Critical and High severity findings
3. **No raw dumps**: Never paste full command output, full file contents, or long dependency lists — summarize and reference
4. **Drill-down on demand**: If the Architect or roundtable needs deeper evidence for a specific finding, the Flight Director spawns a follow-up agent scoped to that single finding

The Flight Director must also enforce this when collecting results — if an agent returns excessively verbose output, summarize it before passing to downstream agents.

### Known Debt Awareness

Cross-reference Inspector findings against known debt from debriefs. Findings that match acknowledged debt should note "previously identified in {debrief}" rather than presenting them as new discoveries. This prevents alarm fatigue.

### Proportional Response

Not every codebase needs a maintenance mission. If the inspection finds only Advisory items, the report should clearly state "Flight Ready" and not push for unnecessary work.

### Honest Assessment

Report what you find, even if the codebase is in great shape. A clean report is valuable — it confirms the team's work quality and builds confidence for the next mission.

### Severity Calibration

- **Critical** is reserved for issues that would cause failures, security vulnerabilities, or data loss
- **Action Required** means the issue will compound or cause problems if left for another cycle
- **Advisory** is for genuine improvements that have no urgency
- **Pass** means the category was inspected and is healthy

## Output

Create the maintenance report artifact using the location and format defined in `.flightops/ARTIFACTS.md`.

After generating the report, summarize:
1. Overall assessment (Flight Ready or Maintenance Required)
2. Count of findings by severity
3. Top recommendations
4. Whether a maintenance mission was scaffolded (and if so, how many flights and legs)
