# Missions

Missions are the human-optimized layer of Flight Control. They define *what* success looks like without prescribing *how* to achieve it.

## What is a Mission?

A mission represents a meaningful outcome—something a stakeholder would recognize as valuable. Missions are:

- **Outcome-driven**: Focused on results, not activities
- **Human-readable**: Written for people, not machines
- **Strategically scoped**: Large enough to matter, bounded enough to complete

### Mission vs. Flight vs. Leg

| Aspect | Mission | Flight | Leg |
|--------|---------|--------|-----|
| Audience | Humans, stakeholders | Developers, AI | AI agents |
| Scope | Outcome | Technical spec | Single task |
| Style | Narrative | Structured | Explicit |
| Duration | Days to weeks | Hours to days | Minutes to hours |

## Writing Effective Missions

### Start with Outcomes

Frame missions around what changes when they're complete:

**Weak** (activity-focused):
> Implement user authentication

**Strong** (outcome-focused):
> Users can securely access their personal data without sharing credentials across services

The outcome framing:
- Clarifies *why* the work matters
- Leaves implementation decisions to flights
- Provides a clear test for completion

### Define Success Criteria

Every mission needs measurable success criteria. These answer: "How do we know we're done?"

```markdown
## Success Criteria

- [ ] Users can create accounts with email/password
- [ ] Users can authenticate via OAuth providers
- [ ] Session management handles concurrent logins
- [ ] Security audit passes with no critical findings
```

Success criteria should be:
- **Observable**: Can be verified by inspection
- **Binary**: Either met or not met
- **Independent**: Achievable without external dependencies
- **Capability-focused**: Describes what users or the system can do, not which tool or technology achieves it

### Consider Stakeholders

Missions serve stakeholders. Identify them explicitly:

```markdown
## Stakeholders

- **End users**: Need frictionless, secure access
- **Security team**: Requires compliance with auth standards
- **Support team**: Needs ability to assist locked-out users
```

Stakeholder identification helps:
- Prioritize competing concerns
- Identify missing success criteria
- Communicate progress meaningfully

## Mission Structure

A mission document typically contains:

```markdown
# Mission: {Title}

## Outcome
What success looks like in human terms.

## Context
Why this mission matters now. Background information.

## Success Criteria
- [ ] Criterion 1
- [ ] Criterion 2
- [ ] Criterion 3

## Stakeholders
Who cares about this outcome and why.

## Constraints
Non-negotiable boundaries (budget, timeline, technology).

## Environment Requirements
Development environment, runtime dependencies, special tooling.

## Open Questions
Unknowns that need resolution during execution.

## Known Issues
Emergent blockers and issues discovered during execution.

## Flights
Links to flights executing this mission.
```

## Mission Lifecycle

Missions progress through defined states:

### States

```
planning ──► active ──► completed
                │
                └──► aborted
```

**planning**
The mission is being defined. Outcome, success criteria, and constraints are still being refined. No flights have started.

**active**
At least one flight is in progress. The mission outcome is being pursued. New flights may be created as understanding develops.

**completed**
All success criteria are met. Stakeholders have accepted the outcome. The mission can be archived.

**aborted**
The mission was cancelled before completion. This might happen due to:
- Changed priorities
- Discovered infeasibility
- External factors

Aborted missions should document *why* they were cancelled for future reference.

### State Transitions

| From | To | Trigger |
|------|----|---------|
| planning | active | First flight begins |
| active | completed | All success criteria met |
| active | aborted | Cancellation decision |
| planning | aborted | Cancellation decision |

## Communicating Mission Status

Missions are stakeholder-facing. Status updates should be meaningful to non-technical audiences:

**Weak update**:
> Completed 3 of 5 flights

**Strong update**:
> Users can now create accounts and log in. Next: adding OAuth support and security review.

Link progress to outcomes, not activities.

## Common Pitfalls

### Too Granular

If a mission can be completed in a single flight, it's probably too small. Consider:
- Is this a meaningful outcome or just a task?
- Would a stakeholder recognize this as valuable?
- Does it warrant its own success criteria?

### Too Vague

Missions need boundaries. "Improve the product" isn't a mission—it's a direction. Missions should be:
- Completable (has an end state)
- Measurable (success criteria exist)
- Bounded (scope is clear)

### Implementation Leaking In

Missions should not prescribe *how*:

**Leaking implementation**:
> Build a React-based authentication flow using JWT tokens stored in HttpOnly cookies

**Proper abstraction**:
> Users can securely authenticate across sessions without re-entering credentials

This applies to success criteria too. Criteria that name tools or technologies lock you into an approach before flights even begin:

**Implementation-specific criteria** (avoid):
> - [ ] JWT tokens are validated via middleware on every request
> - [ ] User records are stored in PostgreSQL with bcrypt-hashed passwords

**Capability-focused criteria** (prefer):
> - [ ] Unauthorized requests are rejected before reaching protected resources
> - [ ] Stored credentials cannot be recovered even if the database is compromised

Save implementation details for flights.

## Mission Debrief

After a mission completes (or aborts), create a **mission debrief** for retrospective learning:

```markdown
# Mission Debrief: {Title}

## Success Criteria Results
Which criteria were met, partially met, or not met.

## Flight Summary
Overview of how each flight contributed.

## What Went Well
Effective patterns and successes.

## What Could Be Improved
Process and execution improvements.

## Lessons Learned
Insights to carry forward.

## Methodology Feedback
Improvements to Flight Control itself.
```

The debrief captures organizational learning and informs future missions.

## Relationship to Flights

Missions spawn flights. A typical mission might have:

```
Mission: Secure User Authentication
├── Flight: Account creation flow
├── Flight: Login and session management
├── Flight: OAuth integration
└── Flight: Security hardening
```

Flights can be planned upfront or emerge as the mission progresses. The mission provides the "why"; flights provide the "what" and "how".

## Next Steps

- [Flights](flights.md) — Learn to create technical specifications
- [Workflow](workflow.md) — See how missions flow into flights
