---
name: mission
description: Create outcome-driven missions through research and user interview. Use when starting a new project, feature, or initiative that needs planning.
---

# Mission Creation

Create a new mission through research and collaborative discovery.

## Prerequisites

- Project must be initialized with `/init-project` (`.flightops/ARTIFACTS.md` must exist)

## Workflow

### Phase 1: Research

Before asking questions, gather context:

1. **Identify the target project**
   - Read `projects.md` to find the project's path
   - If not listed, ask the user for details

2. **Verify project is initialized**
   - Check if `{target-project}/.flightops/ARTIFACTS.md` exists
   - **If missing**: STOP and tell the user to run `/init-project` first
   - Do not proceed without the artifact configuration

3. **Read the artifact configuration**
   - Read `{target-project}/.flightops/ARTIFACTS.md` for artifact locations and formats

4. **Explore the target project's codebase**
   - Project structure and architecture
   - Existing patterns and conventions
   - Related functionality

5. **Read existing documentation**
   - README and project docs
   - Any existing missions
   - Technical specs or design documents

6. **Search external sources if needed**
   - API documentation
   - Library documentation
   - Relevant patterns or best practices

### Phase 2: User Input

Before asking structured questions, share a brief summary of what you learned during research and prompt the user for open-ended input:

- "Here's what I've gathered about the project so far: [summary]. Before I ask specific questions, what are your thoughts on what this mission should include? Feel free to share goals, ideas, concerns, scope preferences — anything that should shape this mission."

Use the user's response to inform and focus the interview questions that follow.

### Phase 3: Interview

Ask about outcomes, not tasks. Focus on:

1. **Desired outcomes**
   - "What does success look like when this is done?"
   - "What problem does this solve for users/stakeholders?"
   - "How will you know this mission succeeded?"

2. **Stakeholders and their needs**
   - "Who benefits from this outcome?"
   - "Are there competing interests to balance?"
   - "Who needs to approve or review?"

3. **Constraints**
   - "What technical constraints exist?"
   - "Are there timeline or resource boundaries?"
   - "What's out of scope?"

4. **Success criteria**
   - "What specific, observable criteria indicate completion?"
   - "How will each criterion be verified?"
   - "Do any criteria name specific tools or technologies? Reframe as capabilities."
   - "Could each criterion be satisfied by more than one implementation approach?"

5. **Environment requirements**
   - "What development environment will be used?"
   - "Are there runtime dependencies?"
   - "What tooling versions are required?"

### Phase 4: Draft

Create the mission artifact using the format defined in `.flightops/ARTIFACTS.md`.

### Phase 4b: Technical Viability Check

Read `{target-project}/.flightops/agent-crews/mission-design.md` for crew definitions and prompts (fall back to defaults at `.claude/skills/init-project/defaults/agent-crews/mission-design.md`).

**Validate structure**: The phase file MUST contain `## Crew`, `## Interaction Protocol`, and `## Prompts` sections with fenced code blocks. If the file exists but is malformed, STOP and tell the user: "Phase file `mission-design.md` is missing required sections. Either fix it manually or re-run `/init-project` to reset to defaults."

1. **Spawn an Architect agent** in the target project context (Task tool, `subagent_type: "general-purpose"`)
   - Provide the "Validate Mission" prompt from the mission-design phase file's Prompts section
   - The Architect reviews the draft mission against the codebase, stack, and constraints
   - The Architect provides a structured assessment: feasible / feasible with caveats / not feasible
2. **Incorporate feedback** — update the mission artifact to address issues raised
   - If not feasible: discuss with user, adjust scope or approach
   - If feasible with caveats: present caveats to user, adjust if needed
3. **Human gives final sign-off** before proceeding

### Phase 5: Iterate

Present the draft and iterate:

1. Walk through each section with the user
2. Validate success criteria are measurable
3. Screen each criterion for technology names, tool names, config file paths, or specific libraries — reframe any that describe implementation rather than capability
4. Confirm flight breakdown makes sense
5. Refine until the user explicitly approves

## Guidelines

### Mission Sizing

A well-sized mission:
- Takes days to weeks to complete
- Spawns 5-7 flights typically
- Represents a meaningful outcome stakeholders recognize
- Has clear success criteria

**Too small**: Can be completed in a single flight
**Too large**: Success criteria are vague or numerous (>10)

### Outcome vs. Activity

Frame missions around results, not tasks:

**Activity-focused** (avoid):
> Implement user authentication

**Outcome-focused** (prefer):
> Users can securely access their personal data without sharing credentials

This applies equally to success criteria. Criteria that name specific tools or technologies constrain the solution space prematurely — if the prescribed tool doesn't work, the criteria become misaligned with reality.

**Implementation-specific criterion** (avoid):
> OAuth tokens are stored in Redis with a 24-hour TTL

**Capability-focused criterion** (prefer):
> Authenticated sessions persist across browser restarts for up to 24 hours

### Alignment Flight

During the interview phase, ask the user whether they'd like to include an alignment flight. Explain that this optional flight is a vibe coding session — the user and agent work together interactively, exploring the codebase, trying ideas, and making hands-on adjustments that benefit from human judgment and real-time feedback. It's a space for creative collaboration rather than structured execution. If the user opts in, include it in the suggested breakdown, marked as optional.

### Adaptive Planning

- Missions can be updated as understanding develops
- New flights can be added during execution
- Success criteria can be refined (with stakeholder agreement)

## Output

Create the mission artifact using the location and format defined in `.flightops/ARTIFACTS.md`.
