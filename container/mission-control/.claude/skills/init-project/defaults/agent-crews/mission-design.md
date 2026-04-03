# Mission Design — Project Crew

Crew definitions for mission planning. The Flight Director interviews the human
and uses project-side agents to validate technical viability.

## Crew

### Architect
- **Context**: {project}/
- **Model**: Sonnet
- **Role**: Validates technical viability of proposed outcomes. Ensures business
  goals align with what's actually possible given the codebase, stack, and
  constraints. Does NOT add implementation details — focuses on feasibility,
  risks, and architectural implications.
- **Actions**: validate-mission

## Interaction Protocol

### Research & Interview
1. Flight Director researches codebase and external context
2. Flight Director interviews human about outcomes, stakeholders, constraints, criteria
3. Human must explicitly sign off before proceeding — iterate until approved

### Technical Viability Check
1. Flight Director spawns **Architect** to review draft mission against codebase
2. Architect evaluates: Are proposed outcomes achievable? Are there technical risks
   the mission doesn't account for? Does the stack support what's being asked?
3. Architect provides assessment — feasible / feasible with caveats / not feasible
4. Flight Director incorporates feedback, re-interviews human if scope changes
5. Human gives final sign-off

## Template Variables

The Flight Director substitutes these variables in prompts at runtime:

| Variable | Description |
|----------|-------------|
| `{project-slug}` | Project identifier from projects.md |

## Prompts

### Architect: Validate Mission

```
role: architect
phase: mission-design
project: {project-slug}
action: validate-mission

Read the draft mission artifact. Cross-reference proposed outcomes and success
criteria against the actual codebase, stack, and project constraints.

Evaluate:
1. Technical feasibility — can the proposed outcomes be achieved with this stack?
2. Architectural implications — does this require significant structural changes?
3. Risk factors — what technical risks could block success?
4. Constraints accuracy — are stated constraints complete and correct?
5. Sizing — is the scope realistic for a mission (days-to-weeks)?

Provide structured output:

**Feasibility**: feasible | feasible with caveats | not feasible

**Risks** (ranked by impact):
- [high/medium/low] Description — mitigation

**Caveats** (if feasible with caveats):
- Description

**Questions** (for the Flight Director):
- Question
```
