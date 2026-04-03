# Flight Logs

Flight logs are living records created during flight execution. They capture what actually happened—when legs started and ended, what changed, decisions made mid-flight, and any deviations or anomalies encountered.

## What is a Flight Log?

A flight log is the authoritative record of a flight's execution. While the flight document describes the *plan*, the flight log documents *reality*. There is exactly one flight log per flight, updated continuously during execution.

**Important**: Flight logs are **append-only** during execution. Never delete or modify existing entries—only add new ones. This preserves the historical record and ensures nothing is lost when revisiting decisions later.

### Flight Log vs. Other Artifacts

| Artifact | Purpose | When Written | Updates |
|----------|---------|--------------|---------|
| Flight | Plan what to do | Before execution | During planning only |
| Flight Log | Record what happened | During execution | Continuously |
| Leg | Instructions for one task | Before execution | Never (immutable) |

## Why Flight Logs Matter

### For Current Execution

Flight logs provide:
- **Continuity**: When work resumes after interruption, the log shows where things stand
- **Decision history**: Why choices were made when the plan didn't fit reality
- **Anomaly tracking**: Issues that didn't block progress but need future attention

### For Future Legs

When creating new legs, the flight log reveals:
- What actually worked vs. what was planned
- Decisions that affect subsequent implementation
- Discovered complexity that should inform estimates
- Patterns or anti-patterns emerging from execution

### For Post-Flight Debrief

The flight log enables meaningful retrospectives by capturing:
- Actual vs. planned progression
- Root causes of deviations
- Learnings that should inform future flights

## Flight Log Structure

```markdown
# Flight Log: {Flight Title}

## Flight Reference
[{Flight Title}](flight.md)

## Summary
Brief overview of the flight's execution status and key outcomes.

---

## Leg Progress

### {Leg Name}
**Status**: completed | landed | in-flight | aborted
**Started**: {timestamp}
**Completed**: {timestamp}

#### Changes Made
- {Summary of what was implemented}
- {Files modified and why}

#### Notes
{Any relevant observations during execution}

---

## Decisions

Choices made during execution that weren't in the original plan.

### {Decision Title}
**Context**: Why this decision was needed
**Options Considered**: What alternatives existed
**Decision**: What was chosen
**Rationale**: Why this choice was made
**Impact**: How this affects the flight or future legs

---

## Deviations

Departures from the planned approach.

### {Deviation Title}
**Planned**: What the flight specified
**Actual**: What was done instead
**Reason**: Why the deviation was necessary
**Outcome**: Result of the deviation

---

## Anomalies

Unexpected issues or behaviors encountered.

### {Anomaly Title}
**Observed**: What happened
**Expected**: What should have happened
**Severity**: blocking | degraded | cosmetic
**Resolution**: How it was handled (or "unresolved")
**Follow-up**: Any actions needed later

---

## Session Notes

Chronological notes from each work session.

### {Date/Session Identifier}
- {Note about progress or observation}
- {Note about what was attempted}
- {Note about what was learned}
```

## When to Update the Flight Log

### Starting a Leg

Record:
- Leg name and start timestamp
- Any context carried from previous legs
- Initial observations about the task

### During Execution

Record:
- Decisions that diverge from or extend the plan
- Unexpected complexity or simplicity
- Anomalies encountered (even if resolved)

### Completing a Leg

Record:
- Completion timestamp
- Summary of changes made
- Files modified
- Any notes for future legs

### Encountering Issues

Record immediately:
- What went wrong
- What was expected
- Severity and impact
- Resolution or workaround

## Using Flight Logs When Creating Legs

When generating new legs with `/leg`, the flight log must be consulted to understand:

1. **What's actually complete**: The log shows real completion status, not just planned progress
2. **Decisions made**: Mid-flight decisions may change how subsequent legs should be implemented
3. **Discovered context**: Anomalies and deviations reveal system behavior that affects new work
4. **Patterns established**: Implementation approaches that worked (or didn't) inform future guidance

### Example: Log Informing Leg Creation

**Flight log entry:**
```markdown
### API Authentication Leg
**Status**: completed

#### Changes Made
- Implemented JWT auth in `src/middleware/auth.ts`
- Used `jose` library instead of planned `jsonwebtoken` (see Decisions)

#### Decisions
**JWT Library Change**
- Context: `jsonwebtoken` had CVE flagged in security scan
- Decision: Switch to `jose` library
- Impact: All subsequent auth-related code must use `jose` patterns
```

**Impact on next leg:**
When creating the "Protected Routes" leg, the guidance must specify using `jose` patterns, not the originally planned `jsonwebtoken` approach. Without the flight log, this context would be lost.

## Location

Flight log location is defined in `.flightops/ARTIFACTS.md`. The artifact system determines where and how logs are stored.

## Best Practices

### Keep Entries Atomic

Write entries as things happen, not in batches. Batched entries lose detail and context.

### Be Factual, Not Editorial

**Good**: "Validation logic required 3 additional edge cases not in spec"
**Avoid**: "The spec was incomplete and caused problems"

### Link to Specifics

Reference file paths, line numbers, commit hashes when relevant. Vague entries lose value quickly.

### Capture the "Why"

The code shows *what* changed. The log should capture *why* decisions were made.

### Don't Duplicate the Leg

The leg document has acceptance criteria and implementation guidance. The log records completion status and what actually happened—not a copy of the plan.

## Common Patterns

### Discovery Pattern

When execution reveals something unexpected:

```markdown
### Discovery: Rate Limiting Already Exists
**Observed**: Found existing rate limiter in `src/middleware/rateLimit.ts`
**Expected**: Needed to implement from scratch per flight spec
**Impact**: Leg `implement-rate-limiting` can be simplified to configuration
**Action**: Updated leg scope, reduced from 4 hours to 30 minutes
```

### Blocker Pattern

When progress stops:

```markdown
### Blocker: Missing API Credentials
**Observed**: Stripe API key not in environment
**Severity**: blocking
**Impact**: Cannot test payment integration
**Resolution**: Requested credentials from DevOps
**Unblocked**: {timestamp when resolved}
```

### Adaptation Pattern

When the plan needs adjustment:

```markdown
### Deviation: Schema Change
**Planned**: Add `verified` boolean to User model
**Actual**: Added `verifiedAt` timestamp instead
**Reason**: Team decided verification time is valuable data
**Outcome**: Provides more information, minor query adjustments needed
```

## Next Steps

- [Flights](flights.md) — Understanding flight structure and lifecycle
- [Legs](legs.md) — How legs are created and executed
- [Workflow](workflow.md) — The complete flow from mission to completion
