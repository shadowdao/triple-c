# Artifact System: Jira

This project stores Flight Control artifacts as Jira issues.

## Issue Type Mapping

| Flight Control | Jira Issue Type | Hierarchy |
|----------------|-----------------|-----------|
| Mission | Epic | Parent |
| Flight | Story | Child of Epic |
| Leg | Sub-task | Child of Story |

## Setup Questions

Answer these questions when configuring Jira artifacts for your project:

| Question | Answer |
|----------|--------|
| What is the Jira project key? | `PROJECT` |
| JQL query for discovering flight documentation? | (e.g., `project = PROJECT AND labels = flight-control`) |

## Configuration

| Property | Value |
|----------|-------|
| Project Key | `PROJECT` |
| Board | (specify board name or ID) |
| Labels | `flight-control` |

---

## Custom Fields

<!-- Add your project's custom Jira fields here -->

| Custom Field | Jira Field ID | Required | Used For | Notes |
|--------------|---------------|----------|----------|-------|
| (example) Team | `customfield_10001` | Yes | All issues | Select from predefined teams |
| (example) Sprint | `customfield_10002` | No | Stories, Sub-tasks | Assign to sprint |

## Project Rules

<!-- Document project-specific Jira rules and conventions here -->

### Required Fields by Issue Type

**Epic (Mission):**
- (list required fields for your project)

**Story (Flight):**
- (list required fields for your project)

**Sub-task (Leg):**
- (list required fields for your project)

### Workflow Rules

- (document any workflow restrictions or automation rules)
- (e.g., "Stories cannot move to In Progress without Epic Link")

### Naming Conventions

- (document any naming patterns required by your project)
- (e.g., "Epic summaries must start with [MISSION]")

---

## Core Artifacts

### Mission → Epic

| Field | Mapping |
|-------|---------|
| Summary | Mission title |
| Description | See format below |
| Labels | `flight-control`, `mission` |

**Description Format:**

```
## Outcome
{What success looks like in human terms}

## Context
{Why this mission matters now}

## Success Criteria
- [ ] {Criterion 1}
- [ ] {Criterion 2}

## Stakeholders
{Who cares about this outcome}

## Constraints
{Non-negotiable boundaries}

## Environment Requirements
{Development and runtime requirements}

## Open Questions
{Unknowns needing resolution}

## Known Issues
Emergent blockers and issues discovered during execution. Add items here as flights surface problems that affect the broader mission — things not anticipated during planning but visible at the mission level.

- [ ] {Issue description} — discovered in Flight {N}, affects {scope}

## Flights
> **Note:** These are tentative suggestions, not commitments. Flights are planned and created one at a time as work progresses. This list will evolve based on discoveries during implementation.

- [ ] Flight 1: {description}
```

---

### Flight → Story

| Field | Mapping |
|-------|---------|
| Summary | Flight title |
| Description | See format below |
| Epic Link | Parent mission epic |
| Labels | `flight-control`, `flight` |

**Description Format:**

```
## Objective
{What this flight accomplishes}

## Contributing to Criteria
- {Relevant success criterion 1}
- {Relevant success criterion 2}

## Design Decisions
{Key technical decisions and rationale}

## Prerequisites
- [ ] {What must be true before execution}

## Technical Approach
{How the objective will be achieved}

## Legs
> **Note:** These are tentative suggestions, not commitments. Legs are planned and created one at a time as the flight progresses. This list will evolve based on discoveries during implementation.

- [ ] {leg-slug} - {description}

## Validation Approach
{How will this flight be validated? Manual testing, automated tests, or both?}

## Verification
{How to confirm success}
```

---

### Leg → Sub-task

| Field | Mapping |
|-------|---------|
| Summary | Leg title |
| Description | See format below |
| Parent | Flight story |
| Labels | `flight-control`, `leg` |

**Description Format:**

```
## Objective
{Single sentence: what this leg accomplishes}

## Context
{Design decisions and learnings from prior legs}

## Inputs
{What must exist before this leg runs}

## Outputs
{What exists after completion}

## Acceptance Criteria
- [ ] {Criterion 1}
- [ ] {Criterion 2}

## Verification Steps
How to confirm each criterion is met:
- {Command or manual check for criterion 1}
- {Command or manual check for criterion 2}

## Implementation Guidance
{Step-by-step guidance}

## Files Affected
{List of files to modify}
```

---

## Supporting Artifacts

### Flight Log → Story Comments

| Property | Value |
|----------|-------|
| Location | Comments on the Flight (Story) |
| Format | Timestamped comments with prefix |
| Update pattern | Append new comments during execution |

**Comment Format:**

```
[Flight Log] {YYYY-MM-DD HH:MM}

## {Entry Type}: {Title}

{Content based on entry type - see below}
```

**Entry Types:**

- `Leg Progress` - Status updates for leg completion
- `Decision` - Runtime decisions not in original plan
- `Deviation` - Departures from planned approach
- `Anomaly` - Unexpected issues encountered
- `Session Notes` - General progress notes

---

### Flight Briefing → Story Comment

| Property | Value |
|----------|-------|
| Location | Comment on the Flight (Story) |
| Created | Before flight execution begins |
| Label | `[Flight Briefing]` |

**Comment Format:**

```
[Flight Briefing] {YYYY-MM-DD}

## Mission Context
{How this flight contributes to mission}

## Objective
{What this flight will accomplish}

## Key Decisions
{Critical decisions crew should know}

## Risks
| Risk | Mitigation |
|------|------------|
| {risk} | {mitigation} |

## Legs Overview
1. {leg} - {description}
2. {leg} - {description}

## Success Criteria
{How we'll know the flight succeeded}
```

---

### Flight Debrief → Story Comment

| Property | Value |
|----------|-------|
| Location | Comment on the Flight (Story) |
| Created | After flight lands or diverts |
| Label | `[Flight Debrief]` |

**Comment Format:**

```
[Flight Debrief] {YYYY-MM-DD}

**Status**: {landed | aborted}
**Duration**: {start} - {end}
**Legs Completed**: {X of Y}

## Outcome Assessment
{What the flight accomplished}

## What Went Well
{Effective patterns}

## What Could Be Improved
{Recommendations}

## Deviations
| Deviation | Reason | Standardize? |
|-----------|--------|--------------|
| {what} | {why} | {yes/no} |

## Key Learnings
{Insights for future flights}

## Recommendations
1. {Most impactful recommendation}
2. {Second recommendation}
3. {Third recommendation}

## Action Items
- [ ] {action}
```

---

### Mission Debrief → Epic Comment

| Property | Value |
|----------|-------|
| Location | Comment on the Mission (Epic) |
| Created | After mission completes or aborts |
| Label | `[Mission Debrief]` |

**Comment Format:**

```
[Mission Debrief] {YYYY-MM-DD}

**Status**: {completed | aborted}
**Duration**: {start} - {end}
**Flights Completed**: {X of Y}

## Success Criteria Results
| Criterion | Status | Notes |
|-----------|--------|-------|
| {criterion} | {met/not met} | {notes} |

## Flight Summary
| Flight | Status | Outcome |
|--------|--------|---------|
| {flight} | {status} | {outcome} |

## What Went Well
{Successes}

## What Could Be Improved
{Improvements}

## Lessons Learned
{Insights}

## Action Items
- [ ] {action}
```

---

## State Mapping

### Mission (Epic)

| Flight Control | Jira Status |
|----------------|-------------|
| planning | To Do |
| active | In Progress |
| completed | Done |
| aborted | Cancelled |

### Flight (Story)

| Flight Control | Jira Status |
|----------------|-------------|
| planning | To Do |
| ready | Ready |
| in-flight | In Progress |
| landed | In Review |
| completed | Done |
| aborted | Cancelled |

### Leg (Sub-task)

| Flight Control | Jira Status |
|----------------|-------------|
| planning | To Do |
| ready | Ready |
| in-flight | In Progress |
| landed | In Review |
| completed | Done |
| aborted | Cancelled |

---

## Conventions

- **Naming**: Use clear, action-oriented summaries
- **Linking**: Always link Stories to Epic, Sub-tasks to Story
- **Labels**: Apply `flight-control` label to all artifacts
- **Immutability**: Never modify Sub-tasks once In Progress; create new ones
- **Comments**: Use prefixes (`[Flight Log]`, `[Flight Briefing]`, etc.) for filtering

## Git Workflow

| Property | Value |
|----------|-------|
| Strategy | `branch` |

**Options:**

- **`branch`** (default) — Single-checkout workflow. The orchestrator creates a feature branch and all agents work in the project root. One flight at a time per working copy.
- **`worktree`** — Worktree isolation. The orchestrator creates a git worktree under `.worktrees/` for each flight. Agents work in the worktree path. Parallel flights are possible on a single repo clone.

When using the `worktree` strategy, add `.worktrees/` to `.gitignore`.
