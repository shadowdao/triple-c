# Artifact System: Filesystem

This project stores Flight Control artifacts as markdown files in the repository.

## Directory Structure

```
{target-project}/
├── missions/
│   └── {NN}-{mission-slug}/
│       ├── mission.md
│       ├── mission-debrief.md
│       └── flights/
│           └── {NN}-{flight-slug}/
│               ├── flight.md
│               ├── flight-log.md
│               ├── flight-briefing.md
│               ├── flight-debrief.md
│               └── legs/
│                   └── {NN}-{leg-slug}.md
└── maintenance/
    └── {YYYY-MM-DD}.md
```

## Naming Conventions

- **Slugs**: Lowercase, kebab-case, derived from title (e.g., "User Authentication" → `user-authentication`)
- **Sequence numbers**: Missions, flights, and legs use two-digit prefixes (`01`, `02`, etc.) for ordering

---

## Core Artifacts

### Mission

| Property | Value |
|----------|-------|
| Location | `missions/{NN}-{slug}/mission.md` |
| Created | During mission planning |
| Updated | Until status changes to `active` |

**Format:**

```markdown
# Mission: {Title}

**Status**: planning | active | completed | aborted

## Outcome
What success looks like in human terms.

## Context
Why this mission matters now. Background information.

## Success Criteria
- [ ] Criterion 1 (observable, binary)
- [ ] Criterion 2
- [ ] Criterion 3

## Stakeholders
Who cares about this outcome and why.

## Constraints
Non-negotiable boundaries.

## Environment Requirements
- Development environment (devcontainer, local toolchain, cloud IDE)
- Runtime requirements (GUI, audio hardware, network access)
- Special tooling (Docker, specific CLI versions)

## Open Questions
Unknowns that need resolution during execution.

## Known Issues
Emergent blockers and issues discovered during execution. Add items here as flights surface problems that affect the broader mission — things not anticipated during planning but visible at the mission level.

- [ ] {Issue description} — discovered in Flight {N}, affects {scope}

## Flights

> **Note:** These are tentative suggestions, not commitments. Flights are planned and created one at a time as work progresses. This list will evolve based on discoveries during implementation.

- [ ] Flight 1: {description}
- [ ] Flight 2: {description}
- [ ] Flight N *(optional)*: Alignment — vibe coding session for creative collaboration and hands-on adjustments
```

---

### Flight

| Property | Value |
|----------|-------|
| Location | `missions/{mission}/flights/{NN}-{slug}/flight.md` |
| Created | During flight planning |
| Updated | Until status changes to `in-flight` |

**Format:**

```markdown
# Flight: {Title}

**Status**: planning | ready | in-flight | landed | completed | aborted
**Mission**: [{Mission Title}](../../mission.md)

## Contributing to Criteria
- [ ] {Relevant success criterion 1}
- [ ] {Relevant success criterion 2}

---

## Pre-Flight

### Objective
What this flight accomplishes (one paragraph).

### Open Questions
- [ ] Question needing resolution
- [x] Resolved question → see Design Decisions

### Design Decisions

**{Decision Title}**: {Choice made}
- Rationale: Why this choice
- Trade-off: What we're giving up

### Prerequisites
- [ ] {What must be true before execution}

### Pre-Flight Checklist
- [ ] All open questions resolved
- [ ] Design decisions documented
- [ ] Prerequisites verified
- [ ] Validation approach defined
- [ ] Legs defined

---

## In-Flight

### Technical Approach
How the objective will be achieved.

### Checkpoints
- [ ] {Milestone 1}
- [ ] {Milestone 2}

### Adaptation Criteria

**Divert if**:
- {Condition requiring re-planning}

**Acceptable variations**:
- {Minor changes that don't require diversion}

### Legs

> **Note:** These are tentative suggestions, not commitments. Legs are planned and created one at a time as the flight progresses. This list will evolve based on discoveries during implementation.

- [ ] `{leg-slug}` - {Brief description}
- [ ] `{leg-slug}` - {Brief description}
- [ ] `hat-and-alignment` *(optional)* - Guided HAT (human acceptance test) session with iterative fixes

---

## Post-Flight

### Completion Checklist
- [ ] All legs completed
- [ ] Code merged
- [ ] Tests passing
- [ ] Documentation updated

### Verification
How to confirm the flight achieved its objective.
```

---

### Leg

| Property | Value |
|----------|-------|
| Location | `missions/{mission}/flights/{flight}/legs/{NN}-{slug}.md` |
| Created | Before leg execution |
| Updated | Never once `in-flight` (immutable) |

**Format:**

```markdown
# Leg: {slug}

**Status**: planning | ready | in-flight | landed | completed | aborted
**Flight**: [{Flight Title}](../flight.md)

## Objective
Single sentence: what this leg accomplishes.

## Context
- Relevant design decisions from the flight
- How this fits into the broader technical approach
- Key learnings from prior legs (from flight log)

## Inputs
What exists before this leg runs:
- Files that must exist
- State that must be true

## Outputs
What exists after this leg completes:
- Files created or modified
- State changes

## Acceptance Criteria
- [ ] Criterion 1 (specific, observable)
- [ ] Criterion 2
- [ ] Criterion 3

## Verification Steps
How to confirm each criterion is met:
- {Command or manual check for criterion 1}
- {Command or manual check for criterion 2}

## Implementation Guidance

1. **{First step}**
   - Details about what to do

2. **{Second step}**
   - Details

## Edge Cases
- **{Edge case 1}**: How to handle

## Files Affected
- `path/to/file.ext` - {What changes}

---

## Post-Completion Checklist

**Complete ALL steps before signaling `[COMPLETE:leg]`:**

- [ ] All acceptance criteria verified
- [ ] Tests passing
- [ ] Update flight-log.md with leg progress entry
- [ ] Set this leg's status to `completed` (in this file's header)
- [ ] Check off this leg in flight.md
- [ ] If final leg of flight:
  - [ ] Update flight.md status to `landed`
  - [ ] Check off flight in mission.md
- [ ] Commit all changes together (code + artifacts)
```

---

## Supporting Artifacts

### Flight Log

| Property | Value |
|----------|-------|
| Location | `missions/{mission}/flights/{flight}/flight-log.md` |
| Created | When flight is created |
| Updated | Continuously during execution (append-only) |

**Format:**

```markdown
# Flight Log: {Flight Title}

**Flight**: [{Flight Title}](flight.md)

## Summary
Brief overview of execution status and key outcomes.

---

## Leg Progress

### {Leg Name}
**Status**: completed | landed | in-flight | aborted
**Started**: {timestamp}
**Completed**: {timestamp}

#### Changes Made
- {Summary of what was implemented}

#### Notes
{Observations during execution}

---

## Decisions
Runtime decisions not in original plan.

### {Decision Title}
**Context**: Why needed
**Decision**: What was chosen
**Impact**: Effect on flight or future legs

---

## Deviations
Departures from planned approach.

### {Deviation Title}
**Planned**: What the flight specified
**Actual**: What was done instead
**Reason**: Why the deviation was necessary

---

## Anomalies
Unexpected issues encountered.

### {Anomaly Title}
**Observed**: What happened
**Severity**: blocking | degraded | cosmetic
**Resolution**: How handled or "unresolved"

---

## Session Notes
Chronological notes from work sessions.
```

---

### Flight Briefing

| Property | Value |
|----------|-------|
| Location | `missions/{mission}/flights/{flight}/flight-briefing.md` |
| Created | Before flight execution begins |
| Purpose | Pre-flight summary for crew alignment |

**Format:**

```markdown
# Flight Briefing: {Flight Title}

**Date**: {briefing date}
**Flight**: [{Flight Title}](flight.md)
**Status**: Flight is ready for execution

## Mission Context
{Brief reminder of mission outcome and how this flight contributes}

## Objective
{What this flight will accomplish}

## Key Decisions
{Summary of critical design decisions crew should know}

## Risks and Mitigations
| Risk | Mitigation |
|------|------------|
| {risk} | {mitigation} |

## Legs Overview
1. `{leg-slug}` - {description} - {estimated complexity}
2. `{leg-slug}` - {description} - {estimated complexity}

## Environment Requirements
{Any special setup needed before starting}

## Success Criteria
{How we'll know the flight succeeded}
```

---

### Flight Debrief

| Property | Value |
|----------|-------|
| Location | `missions/{mission}/flights/{flight}/flight-debrief.md` |
| Created | After flight lands or diverts |
| Purpose | Post-flight analysis and lessons learned |

**Format:**

```markdown
# Flight Debrief: {Flight Title}

**Date**: {debrief date}
**Flight**: [{Flight Title}](flight.md)
**Status**: {landed | aborted}
**Duration**: {start} - {end}
**Legs Completed**: {X of Y}

## Outcome Assessment

### Objectives Achieved
{What the flight accomplished}

### Mission Criteria Advanced
{Which success criteria this flight contributed to}

## What Went Well
{Specific things that worked effectively}

## What Could Be Improved

### Process
- {Recommendations for flight execution}

### Technical
- {Code quality, architecture, debt}

### Documentation
- {Gaps identified}

## Deviations and Lessons Learned

| Deviation | Reason | Standardize? |
|-----------|--------|--------------|
| {what changed} | {why} | {yes/no} |

## Key Learnings
{Insights for future flights}

## Recommendations
1. {Most impactful recommendation}
2. {Second recommendation}
3. {Third recommendation}

## Action Items
- [ ] {Immediate actions}
- [ ] {Near-term improvements}
```

---

### Mission Debrief

| Property | Value |
|----------|-------|
| Location | `missions/{NN}-{mission}/mission-debrief.md` |
| Created | After mission completes or aborts |
| Purpose | Post-mission retrospective and methodology improvements |

**Format:**

```markdown
# Mission Debrief: {Mission Title}

**Date**: {debrief date}
**Mission**: [{Mission Title}](mission.md)
**Status**: {completed | aborted}
**Duration**: {start} - {end}
**Flights Completed**: {X of Y}

## Outcome Assessment

### Success Criteria Results
| Criterion | Status | Notes |
|-----------|--------|-------|
| {criterion} | {met/not met} | {notes} |

### Overall Outcome
{Did we achieve what we set out to do?}

## Flight Summary
| Flight | Status | Key Outcome |
|--------|--------|-------------|
| {flight} | {landed/aborted} | {outcome} |

## What Went Well
{Effective patterns and successes}

## What Could Be Improved
{Process, planning, execution improvements}

## Lessons Learned
{Insights to carry forward}

## Methodology Feedback
{Improvements to Flight Control process itself}

## Action Items
- [ ] {Follow-up work}
- [ ] {Process improvements}
```

---

### Maintenance Report

| Property | Value |
|----------|-------|
| Location | `maintenance/{YYYY-MM-DD}.md` |
| Created | After a mission or ad-hoc, during routine maintenance |
| Purpose | Codebase health assessment and maintenance recommendation |

**Format:**

```markdown
# Maintenance Report: {YYYY-MM-DD}

**Date**: {report date}
**Triggered by**: [{Mission Title}](missions/{NN}-{slug}/mission.md) *(optional — omit if ad-hoc)*
**Assessment**: {Flight Ready | Maintenance Required}

## Categories Inspected
{Numbered list of categories that were checked}

## Executive Summary
{2-3 sentence overview of codebase health and key findings}

## Findings by Category

### Category {N}: {Name}

| # | Finding | Severity | New/Known | Recommendation |
|---|---------|----------|-----------|----------------|
| {n} | {title} | {severity} | {new/known} | {recommendation} |

**Details:**
{Per-finding evidence with file paths and line numbers}

## Severity Summary

| Severity | Count |
|----------|-------|
| Critical | {N} |
| Action Required | {N} |
| Advisory | {N} |
| Pass | {N} |

## Known Debt Carried Forward
{Debt items from debriefs that were acknowledged but not addressed, or "None — no prior debt context"}

## Recommendations
1. {Most impactful recommendation}
2. {Second recommendation}
3. {Third recommendation}

## Maintenance Mission
{Link to scaffolded mission if created, or "Not required — codebase is flight ready"}
```

---

## State Tracking

States are tracked in the frontmatter or status field of each artifact:

| Artifact | States |
|----------|--------|
| Mission | `planning` → `active` → `completed` (or `aborted`) |
| Flight | `planning` → `ready` → `in-flight` → `landed` → `completed` (or `aborted`) |
| Leg | `planning` → `ready` → `in-flight` → `landed` → `completed` (or `aborted`) |

## Conventions

- **Immutability**: Never modify legs once `in-flight`; create new ones instead
- **Append-only logs**: Flight logs are append-only during execution
- **Flight briefings**: Created before execution, not modified after
- **Debriefs**: Created after completion, may be updated with follow-up notes
- **Mission as briefing**: The mission.md document serves as both definition and briefing (no separate mission-briefing.md)
