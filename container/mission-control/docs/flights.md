# Flights

Flights are the balanced layer of Flight Control—technical enough for implementation, readable enough for humans. A flight and its flight plan are synonymous: the document *is* the plan.

## What is a Flight?

A flight translates mission outcomes into technical specifications. Flights are:

- **Technically scoped**: Bounded implementation work
- **Checklist-driven**: Pre-flight, in-flight, and post-flight phases
- **Adaptive**: Living documents that evolve with understanding

### Flight vs. Mission vs. Leg

| Aspect | Mission | Flight | Leg |
|--------|---------|--------|-----|
| Question | Why? | What & How? | Do this |
| Audience | Stakeholders | Developers/AI | AI agents |
| Flexibility | High | Medium | Low |
| Updates | Rarely | As needed | Never (create new) |

## Flight Structure

Every flight has three sections corresponding to its lifecycle:

```markdown
# Flight: {Title}

## Mission Link
Parent mission and relevant success criteria.

---

## Pre-Flight

### Objective
What this flight accomplishes.

### Open Questions
- [ ] Question needing resolution
- [ ] Another question

### Design Decisions
Choices made and their rationale.

### Prerequisites
What must be true before execution begins.

### Pre-Flight Checklist
- [ ] Questions resolved
- [ ] Design decisions documented
- [ ] Prerequisites verified
- [ ] Legs defined

---

## In-Flight

### Technical Approach
How the objective will be achieved.

### Checkpoints
Key milestones during execution.

### Adaptation Criteria
When to deviate from plan.

### Legs
Links to implementation legs.

---

## Post-Flight

### Completion Checklist
- [ ] All legs completed
- [ ] Integration verified
- [ ] Tests passing
- [ ] Documentation updated

### Verification
How to confirm the flight achieved its objective.

### Retrospective Notes
What was learned during this flight.
```

## Pre-Flight Phase

Pre-flight is about readiness. No implementation happens here—only planning.

### Open Questions

Capture unknowns that need resolution before execution:

```markdown
### Open Questions
- [ ] Should we use JWT or session-based auth?
- [ ] What OAuth providers do we need to support?
- [ ] How long should sessions remain valid?
```

Questions should be:
- **Specific**: Clear what information is needed
- **Blocking**: Execution can't proceed without answers
- **Resolvable**: Someone can answer them

Check off questions as they're resolved and document answers in Design Decisions.

### Design Decisions

Record choices and rationale:

```markdown
### Design Decisions

**Authentication Method**: JWT tokens
- Rationale: Stateless, works across services
- Trade-off: Token revocation more complex
- Decided by: Security team review

**Session Duration**: 7 days with refresh
- Rationale: Balance security and convenience
- Trade-off: Longer exposure window
- Decided by: Product requirements
```

Design decisions prevent relitigating settled questions and help future maintainers understand *why*.

### Prerequisites

List conditions required for execution:

```markdown
### Prerequisites
- [ ] Database schema migrations ready
- [ ] API authentication middleware exists
- [ ] Test environment configured
- [ ] Security review of approach completed
```

Prerequisites are external dependencies. If they're not met, the flight can't begin safely.

### Pre-Flight Checklist

The gate check before execution:

```markdown
### Pre-Flight Checklist
- [ ] All open questions resolved
- [ ] Design decisions documented with rationale
- [ ] Prerequisites verified
- [ ] Legs defined with acceptance criteria
- [ ] Estimated scope is reasonable
```

When all items are checked, the flight moves from `planning` to `ready`.

## Flight Briefing

Before execution begins, create a **flight briefing** to align the crew:

```markdown
# Flight Briefing: {Title}

## Mission Context
How this flight contributes to the mission.

## Objective
What this flight will accomplish.

## Key Decisions
Critical design decisions the crew should know.

## Risks
Known risks and mitigation strategies.

## Legs Overview
Summary of legs with complexity notes.

## Success Criteria
How we'll know the flight succeeded.
```

The briefing is created when the flight moves to `ready` status.

## In-Flight Phase

In-flight is execution. Legs are being worked, progress is tracked in the [flight log](flight-logs.md).

### Technical Approach

Document *how* the objective will be achieved:

```markdown
### Technical Approach

1. Create user model with email/password fields
2. Implement registration endpoint with validation
3. Add login endpoint with JWT generation
4. Create middleware for protected routes
5. Add password reset flow
```

This bridges design decisions to concrete implementation without being leg-level specific.

### Checkpoints

Define milestones for progress tracking:

```markdown
### Checkpoints
- [ ] User registration working end-to-end
- [ ] Login returning valid tokens
- [ ] Protected routes rejecting invalid tokens
- [ ] Password reset emails sending
```

Checkpoints help identify if a flight is on track or needs intervention.

### Adaptation Criteria

When should the plan change?

```markdown
### Adaptation Criteria

**Divert if**:
- Security vulnerability discovered in approach
- External API changes break integration
- Performance testing reveals blocking issues

**Acceptable variations**:
- Minor API changes for developer experience
- Additional validation rules
- Extended error handling
```

Explicit adaptation criteria prevent both rigid adherence to bad plans and unnecessary scope creep.

### Legs

Reference the implementation legs:

```markdown
### Legs
- [x] `create-user-model` - completed
- [x] `registration-endpoint` - completed
- [ ] `login-endpoint` - in-flight
- [ ] `auth-middleware` - planning
- [ ] `password-reset` - planning
```

### Flight Log

During execution, maintain a [flight log](flight-logs.md) alongside this document. The flight log records:

- **Leg progress**: When legs start and complete, with summaries of changes
- **Decisions**: Choices made during execution not in the original plan
- **Deviations**: Departures from the planned approach and why
- **Anomalies**: Unexpected issues or behaviors encountered

The flight log is essential for:
- Providing continuity when work resumes after interruption
- Informing the creation of subsequent legs
- Enabling meaningful post-flight retrospectives

## Post-Flight Phase

Post-flight is verification and learning. Implementation is complete; now confirm success.

### Completion Checklist

Verify all work is finished:

```markdown
### Completion Checklist
- [ ] All legs marked completed
- [ ] Code merged to main branch
- [ ] Integration tests passing
- [ ] Documentation updated
- [ ] No blocking issues remaining
```

### Verification

How to confirm the flight succeeded:

```markdown
### Verification

**Manual verification**:
1. Create new user account
2. Log in with credentials
3. Access protected endpoint
4. Log out and verify token invalid

**Automated verification**:
- `npm run test:auth` passes
- `npm run test:e2e` auth flows pass
```

### Retrospective Notes

Capture learnings for future flights:

```markdown
### Retrospective Notes

**What went well**:
- JWT library well-documented
- Security review caught edge case early

**What could improve**:
- Underestimated password reset complexity
- Should have spiked OAuth earlier

**For next time**:
- Include OAuth research in pre-flight
- Add explicit leg for error handling
```

## Flight Lifecycle

Flights progress through defined states:

### States

```
planning ──► ready ──► in-flight ──► landed ──► completed
                           │
                           └──► aborted
```

**planning**
Pre-flight phase. Questions being resolved, design decisions being made.

**ready**
Pre-flight checklist complete. All prerequisites met. Ready to execute.

**in-flight**
Legs actively being executed. Checkpoints being reached. Flights may be modified during this phase (e.g., changing planned legs) as long as the flight log captures the change and rationale.

**landed**
All legs complete. Verification passed. Flight achieved its objective. Ready for debrief.

**completed**
Debrief done. Flight fully wrapped up and artifacts updated.

**aborted**
Flight cancelled. Changes are rolled back. Document the reason for future reference.

### In-Flight Modifications

Flights can be modified while `in-flight` — for example, when planned legs need to change due to discoveries during execution. This is not a separate state; simply update the flight artifact and record the change and rationale in the flight log.

**Create a new flight when:**
- A completely new objective emerges
- The discovered work is independent of the current flight's goal
- The new work serves different mission success criteria

### State Transitions

| From | To | Trigger |
|------|----|---------|
| planning | ready | Pre-flight checklist complete |
| ready | in-flight | First leg begins |
| in-flight | landed | All legs complete |
| in-flight | aborted | Flight cancelled, changes rolled back |
| landed | completed | Debrief complete |

## Connecting to Parent Mission

Flights serve missions. Each flight should clearly link to:

```markdown
## Mission Link

**Mission**: [Secure User Authentication](../mission.md)

**Contributing to criteria**:
- [ ] Users can create accounts with email/password
- [ ] Session management handles concurrent logins
```

This traceability ensures flights aren't orphaned work—they connect to meaningful outcomes.

## Connecting to Child Legs

Flights generate legs. The flight defines *what* needs to happen; legs define the *exact* implementation steps.

A well-structured flight enables AI agents to execute legs without needing clarification:

**Flight provides**:
- Technical approach and patterns
- Design decisions and constraints
- Context about the broader system

**Legs consume**:
- Specific task to complete
- Acceptance criteria to meet
- Context from flight document

## Common Pitfalls

### Skipping Pre-Flight

Rushing to implementation creates problems. Pre-flight exists to:
- Surface hidden complexity
- Align on approach before work begins
- Identify blocking dependencies

A thorough pre-flight prevents mid-flight crises.

### Over-Specifying

Flights should guide, not constrain. Leave room for:
- Implementation details discovered during work
- Better approaches found during execution
- Minor adjustments that don't affect outcomes

### Ignoring Adaptation Criteria

When circumstances change, adapt the plan. Flights that ignore reality become fiction. Use adaptation criteria to decide when to divert.

### Skipping Post-Flight

Post-flight isn't bureaucracy—it's learning. Retrospective notes prevent repeating mistakes. Verification confirms the flight actually succeeded.

## Flight Debrief

After a flight lands (or diverts), create a **flight debrief** for retrospective analysis:

```markdown
# Flight Debrief: {Title}

## Outcome Assessment
What the flight accomplished and which mission criteria it advanced.

## What Went Well
Effective patterns during execution.

## What Could Be Improved
Process and technical recommendations.

## Deviations and Lessons
What changed from the plan and why.

## Recommendations
Top 3-5 most impactful improvements.

## Action Items
Follow-up work and improvements.
```

The debrief enables continuous improvement and informs future flights.

## Next Steps

- [Flight Logs](flight-logs.md) — Recording execution progress and decisions
- [Legs](legs.md) — Learn to write AI-optimized implementation steps
- [Workflow](workflow.md) — See how flights flow into legs
