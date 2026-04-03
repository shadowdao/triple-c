---
name: flight
description: Create technical flight specifications from missions. Use when breaking down a mission into implementable work or planning technical approach.
---

# Flight Specification

Create a technical flight spec from a mission.

## Prerequisites

- Project must be initialized with `/init-project` (`.flightops/ARTIFACTS.md` must exist)
- A mission must exist before creating a flight

## Workflow

### Phase 1: Context Gathering

1. **Identify the target project**
   - Read `projects.md` to find the project's path

2. **Verify project is initialized**
   - Check if `{target-project}/.flightops/ARTIFACTS.md` exists
   - **If missing**: STOP and tell the user to run `/init-project` first
   - Do not proceed without the artifact configuration

3. **Read the artifact configuration**
   - Read `{target-project}/.flightops/ARTIFACTS.md` for artifact locations and formats

4. **Read the parent mission**
   - Understand the outcome being pursued
   - Identify which success criteria this flight addresses
   - Note constraints that apply

5. **Check existing flights**
   - What flights already exist for this mission?
   - What's been completed vs. in progress?
   - Are there dependencies on other flights?

### Phase 2: Code Interrogation

Explore the target project's codebase to inform the technical approach:

1. **Identify relevant code areas**
   - What existing code relates to this flight?
   - What patterns are already established?
   - What dependencies exist?

2. **Find files likely to be affected**
   - Source files to modify
   - Test files to create/update
   - Configuration changes needed

3. **Understand existing patterns**
   - Code style and conventions
   - Error handling approaches
   - Testing patterns

4. **Check for schema/migration implications**
   - Does this flight add or modify database tables?
   - What migration tooling does the project use?
   - Are there existing migration patterns to follow?

### Phase 3: User Input

Before asking structured technical questions, share a brief summary of what you learned during context gathering and code interrogation, then prompt the user for open-ended input:

- "Here's what I've gathered about the mission context and codebase: [summary]. Before I ask specific technical questions, what are your thoughts on what this flight should cover? Feel free to share approach preferences, priorities, concerns — anything that should shape this flight."

Use the user's response to inform and focus the crew interview questions that follow.

### Phase 4: Crew Interview

Ask technical questions to resolve the approach:

1. **Technical approach**
   - "Should we extend existing code or create new modules?"
   - "What's the preferred pattern for [specific decision]?"
   - "Are there performance considerations?"

2. **Open questions**
   - Surface ambiguities in requirements
   - Clarify edge cases
   - Identify unknowns

3. **Design decisions**
   - Document choices and rationale
   - Get agreement on trade-offs
   - Note constraints discovered

4. **Prerequisites verification**
   - "Is [dependency] ready?"
   - "Do we have access to [resource]?"

5. **Validation approach**
   - "How will this flight be validated?"
   - "Is test automation needed, or is manual verification sufficient?"
   - "What tests should be created or updated?"

### Phase 5: Spec Creation

Create the flight artifact using the format defined in `.flightops/ARTIFACTS.md`.

Also create the flight log artifact (empty, ready for execution notes).

### Phase 5b: Design Review

Spawn an Architect agent to validate the flight spec against the real codebase before presenting it to the crew.

Read `{target-project}/.flightops/agent-crews/flight-design.md` for crew definitions and prompts (fall back to defaults at `.claude/skills/init-project/defaults/agent-crews/flight-design.md`).

**Validate structure**: The phase file MUST contain `## Crew`, `## Interaction Protocol`, and `## Prompts` sections with fenced code blocks. If the file exists but is malformed, STOP and tell the user: "Phase file `flight-design.md` is missing required sections. Either fix it manually or re-run `/init-project` to reset to defaults."

1. **Spawn an Architect agent** in the target project context (Task tool, `subagent_type: "general-purpose"`)
   - Provide the "Review Flight Design" prompt from the flight-design phase file's Prompts section
   - The Architect reads the flight spec and cross-references design decisions, prerequisites, technical approach, and leg breakdown against actual codebase state and architecture best practices
   - The Architect provides a structured assessment: approve, approve with changes, or needs rework
2. **Incorporate feedback** — update the flight artifact to address issues raised
   - High-severity issues: must fix before proceeding
   - Medium-severity issues: fix unless there's a clear reason not to
   - Low-severity issues and suggestions: apply at discretion
3. **Re-review if substantive changes were made** — spawn another Architect for a second pass
   - Skip if only minor/cosmetic fixes were applied
   - **Max 2 design review cycles** — if issues persist after 2 rounds, escalate to human
4. **Proceed to Phase 6** with a codebase-validated spec

### Phase 6: Iterate

1. Walk through the spec with the crew
2. Validate technical approach is sound
3. Confirm leg breakdown is appropriate
4. Refine until approved

## Guidelines

### Flight Sizing

A well-sized flight:
- Takes 1-3 days of focused work
- Breaks into 3-8 legs typically
- Has a clear, verifiable objective
- Addresses specific mission criteria

**Too small**: Single leg's worth of work
**Too large**: More than a week of work, vague checkpoints

### Leg Identification

Break flights into legs based on technical boundaries:
- Each leg should be atomic (independently completable)
- Legs should have clear inputs and outputs
- Consider dependencies between legs
- Group related changes together

**For scaffolding flights**: Include a final `verify-integration` leg

**For interface changes**: Identify consumers that need updates

**For documentation**: Consider whether README, CLAUDE.md, or other docs need updates as part of this flight — especially for flights adding new CLI commands, API endpoints, or configuration options

**For schema changes**: Include explicit migration legs and verify against the live database, not just mocks

**For UAT and alignment**: During the crew interview, ask the user whether they'd like to include a UAT and alignment leg. Explain that this optional leg is a guided UAT session — the agent walks the user through a series of tests and verification steps, fixing issues along the way until the user is satisfied with the results. If the user opts in, include it in the breakdown, marked as optional.

### Pre-Flight Rigor

- Open questions MUST be resolved before execution
- Design decisions MUST be documented with rationale
- Prerequisites MUST be verified, not assumed
- **Environment conflicts**: Flights introducing network services (ports, databases, containers) must check for conflicts with existing services on the developer's machine during planning. Ask: "What else is running that could conflict?"

### Adaptive Planning

- Flights can be modified during `planning` state
- Once `in-flight`, flights may still be modified (e.g., changing planned legs) as long as the flight log captures the change and rationale
- New legs can be added if scope grows

## Output

Create the following artifacts using locations and formats from `.flightops/ARTIFACTS.md`:

1. **Flight spec** — The flight plan
2. **Flight log** — Empty, ready for execution notes
