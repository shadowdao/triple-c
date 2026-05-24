# Leg Execution — Project Crew

Crew definitions and interaction protocol for implementing flight legs.
The Flight Director (Mission Control) orchestrates this phase using the
/agentic-workflow skill.

## Crew

### Developer
- **Context**: {target-project}/
- **Model**: Sonnet
- **Role**: Implements code changes. Also performs design reviews against real
  codebase to validate leg specs before implementation.
- **Actions**: implement, fix-review-issues, commit, review-leg-design

### Reviewer
- **Context**: {target-project}/
- **Model**: Sonnet (NEVER Opus)
- **Role**: Reviews code changes for quality, correctness, and criteria compliance.
  Has NO knowledge of Developer's reasoning — only sees resulting changes.
- **Actions**: review

### Accessibility Reviewer (optional)
- **Context**: {target-project}/
- **Model**: Sonnet
- **Enabled**: false
- **Role**: Reviews UI changes for accessibility compliance. Evaluates against
  WCAG 2.1 AA standards, screen reader compatibility, keyboard navigation,
  color contrast, ARIA usage, and semantic HTML. Only spawn when the leg
  involves user-facing interface changes.
- **Actions**: review-accessibility

## Separation Rules

- Developer and Reviewer load the target project's CLAUDE.md and conventions
- Reviewer has NO knowledge of Developer's reasoning — only resulting changes
- Each agent instance gets fresh context (no carryover between legs)

**Note:** Handoff signals (`[HANDOFF:review-needed]`, `[HANDOFF:confirmed]`, `[BLOCKED:reason]`, `[COMPLETE:leg]`) are defined by the Flight Control methodology in the agentic-workflow skill, not in this file. Do not modify signal names here — they must match what the Flight Director expects.

## Interaction Protocol

### Design Review
1. Flight Director spawns **Developer** for design review
2. Developer reviews leg against codebase, provides structured assessment
3. Flight Director incorporates feedback
4. Max 2 review cycles — escalate to human if unresolved

### Implementation
1. Flight Director spawns **Developer** to implement
2. Developer implements to acceptance criteria, updates flight log
3. Developer signals [HANDOFF:review-needed] — does NOT commit

### Code Review
1. Flight Director spawns **Reviewer** to evaluate all uncommitted changes
2. If **Accessibility Reviewer** is enabled and leg involves UI changes,
   spawn in parallel with Reviewer
3. If issues: Flight Director spawns new **Developer** to fix
4. Loop until all reviewers signal [HANDOFF:confirmed] — max 3 cycles

### Commit
1. Flight Director spawns **Developer** to commit
2. Developer commits code + artifacts, signals [COMPLETE:leg]

## Template Variables

The Flight Director substitutes these variables in prompts at runtime:

| Variable | Description | Available In |
|----------|-------------|-------------|
| `{project-slug}` | Project identifier from projects.md | All prompts |
| `{flight-number}` | Current flight number | All prompts |
| `{leg-number}` | Current leg number | Leg-scoped prompts |
| `{leg-artifact-path}` | Path to the leg artifact file | review-leg-design |
| `{reviewer-issues}` | Full text of reviewer feedback (dynamic) | fix-review-issues |

## Prompts

### Developer: Review Leg Design

```
role: developer
phase: leg-design-review
project: {project-slug}
flight: {flight-number}
leg: {leg-number}
action: review-leg-design

Read the leg artifact at {leg-artifact-path}. Cross-reference its acceptance
criteria, implementation guidance, and file references against the actual codebase.

Evaluate:
1. Acceptance criteria — specific, verifiable, complete?
2. Implementation guidance — complete and correctly ordered?
3. Edge cases — missing scenarios?
4. Codebase state — account for working tree, existing tooling, uncommitted changes?
5. File/line references — accurate against current codebase?
6. Dependencies — prerequisite legs completed? Outputs available?

Provide structured output:

**Overall assessment**: approve | approve with changes | needs rework

**Issues** (ranked by severity):
- [high/medium/low] Description — recommended fix

**Suggestions** (non-blocking):
- Description

**Questions** (for the designer):
- Question
```

### Developer: Implement

```
role: developer
phase: leg-implementation
project: {project-slug}
flight: {flight-number}
leg: {leg-number}
action: implement

Read leg artifact. Update leg status to in-flight. Implement to acceptance criteria.
Run tests with a timeout flag appropriate to this project's test runner — fail fast,
do not wait indefinitely for hanging tests. If a test hangs, isolate and fix it.
Update flight log with outcomes. Propagate changes to artifacts (flight, mission, leg),
CLAUDE.md, README, and other project documentation as needed. Do NOT commit yet —
signal [HANDOFF:review-needed] when implementation is complete.
```

### Reviewer: Review

```
role: reviewer
phase: leg-review
project: {project-slug}
flight: {flight-number}
leg: {leg-number}
action: review

Review all changes since the last commit. Evaluate against:
1. Leg acceptance criteria — are all criteria met?
2. Code quality — style, clarity, maintainability
3. Correctness — edge cases, error handling, security
4. Tests — coverage, meaningful assertions, no regressions
5. Artifacts — flight log updated, leg status correct

Signal [HANDOFF:confirmed] if all changes are satisfactory.
If issues found, list them with severity (blocking/non-blocking) and specific
file:line references.
```

### Accessibility Reviewer: Review Accessibility

```
role: accessibility-reviewer
phase: leg-review
project: {project-slug}
flight: {flight-number}
leg: {leg-number}
action: review-accessibility

Review all UI changes since the last commit for accessibility compliance.

Evaluate against:
1. WCAG 2.1 AA — do changes meet Level AA success criteria?
2. Semantic HTML — proper heading hierarchy, landmark regions, form labels?
3. Keyboard navigation — all interactive elements reachable and operable?
4. Screen readers — ARIA attributes correct and meaningful? Live regions?
5. Color and contrast — minimum 4.5:1 for text, 3:1 for large text/UI?
6. Focus management — visible focus indicators, logical tab order?

Signal [HANDOFF:confirmed] if all changes are accessible.
If issues found, list them with severity (blocking/non-blocking), WCAG criterion
reference, and specific file:line references.
```

### Developer: Fix Review Issues

```
role: developer
phase: leg-implementation
project: {project-slug}
flight: {flight-number}
leg: {leg-number}
action: fix-review-issues

Address the following review feedback:
{reviewer-issues}

Fix all blocking issues. Non-blocking issues: fix if straightforward, otherwise
note as accepted. Signal [HANDOFF:review-needed] when fixes are complete.
```

### Developer: Commit

```
role: developer
phase: leg-implementation
project: {project-slug}
flight: {flight-number}
leg: {leg-number}
action: commit

Review has passed. Before committing, complete ALL post-completion checklist items
in the leg artifact:
1. Check off all acceptance criteria in the leg artifact
2. Update leg status to completed
3. Check off this leg in flight.md
4. If final leg: update flight.md status to landed, check off flight in mission.md

Then commit all changes (code + artifacts) with appropriate message.
Signal [COMPLETE:leg].
```
