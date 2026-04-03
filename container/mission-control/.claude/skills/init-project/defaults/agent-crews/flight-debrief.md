# Flight Debrief — Project Crew

Crew definitions for post-flight analysis. The Flight Director interviews the
human and project-side agents to capture both execution and design perspectives.

## Crew

### Developer
- **Context**: {project}/
- **Model**: Sonnet
- **Role**: Provides developer perspective on flight execution. Reviews what was
  built, identifies technical debt introduced, evaluates implementation quality,
  and surfaces issues that flight logs may not capture.
- **Actions**: debrief-interview

### Architect
- **Context**: {project}/
- **Model**: Sonnet
- **Role**: Closes the design feedback loop. Evaluates whether the design decisions
  made during flight planning held up in practice. Reviews architectural impact of
  what was built and whether the approach should be adjusted for future flights.
- **Actions**: debrief-design-review

## Interaction Protocol

### Developer Interview
1. Flight Director loads flight context (mission, flight, legs, log, actual code)
2. Flight Director spawns **Developer** to review implementation and provide feedback
3. Developer examines code changes, test coverage, patterns used, debt introduced
4. Developer provides structured debrief input

### Architect Interview
1. Flight Director spawns **Architect** to review whether design decisions held up
2. Architect compares flight-design spec against actual implementation
3. Architect evaluates architectural impact and provides feedback for future flights

### Human Interview
1. Flight Director interviews human with targeted questions based on flight log
2. Keep lightweight — 2-3 questions max

### Synthesis
1. Flight Director synthesizes Developer input + Architect input + human input + document analysis
2. Generates debrief artifact

## Template Variables

The Flight Director substitutes these variables in prompts at runtime:

| Variable | Description |
|----------|-------------|
| `{project-slug}` | Project identifier from projects.md |
| `{flight-number}` | Current flight number |
| `{flight-artifact-path}` | Path to the flight artifact file |

## Prompts

### Developer: Debrief Interview

```
role: developer
phase: flight-debrief
project: {project-slug}
flight: {flight-number}
action: debrief-interview

Review the implementation produced during this flight. Examine the code changes,
test coverage, and architectural decisions made.

Provide structured input for the debrief:

**Implementation Quality**:
- Does the code follow project conventions?
- Are there patterns that should be documented?
- What technical debt was introduced?

**Leg Spec Accuracy**:
- Were leg specs clear and sufficient for implementation?
- What was missing or misleading?
- Were acceptance criteria verifiable?

**Testing Assessment**:
- Is test coverage adequate?
- Are there untested edge cases?
- Do tests meaningfully validate behavior?

**Recommendations**:
- What should future flights in this area account for?
- Are there refactoring opportunities?
- What documentation is missing?
```

### Architect: Debrief Design Review

```
role: architect
phase: flight-debrief
project: {project-slug}
flight: {flight-number}
action: debrief-design-review

Read the flight artifact at {flight-artifact-path}. Compare the design decisions,
technical approach, and leg breakdown that were planned against what was actually
implemented.

Provide structured input for the debrief:

**Design Decisions Assessment**:
- Which design decisions held up well in practice?
- Which decisions had to be revised during implementation? Why?
- Were there decisions that should have been made differently?

**Architectural Impact**:
- Did the implementation maintain or improve the system's architecture?
- Were there unplanned structural changes? Are they sound?
- Did the approach create any architectural debt?

**Flight Design Accuracy**:
- Was the technical approach feasible as specified?
- Were prerequisites correctly identified?
- Was the leg breakdown appropriate for the actual work?

**Forward-Looking**:
- What should future flight designs in this area account for?
- Are there architectural patterns that emerged worth standardizing?
- What design assumptions should be revisited?
```
