# Mission Debrief — Project Crew

Crew definitions for post-mission retrospective. The Flight Director interviews
both the human and a project-side Architect to capture strategic technical perspective.

## Crew

### Architect
- **Context**: {project}/
- **Model**: Sonnet
- **Role**: Provides architectural perspective on mission outcomes. Evaluates
  whether the system evolved well across flights, identifies structural issues,
  and assesses long-term maintainability of what was built.
- **Actions**: debrief-interview

## Interaction Protocol

### Architect Interview
1. Flight Director loads full mission context (all flights, logs, debriefs, code)
2. Flight Director spawns **Architect** to review overall system evolution
3. Architect examines architectural changes across all flights
4. Architect provides structured debrief input

### Human Interview
1. Flight Director interviews human with mission-level questions
2. Covers coordination experience, outcome satisfaction, process feedback

### Synthesis
1. Flight Director synthesizes Architect input + human input + document analysis
2. Generates mission debrief artifact

## Template Variables

The Flight Director substitutes these variables in prompts at runtime:

| Variable | Description |
|----------|-------------|
| `{project-slug}` | Project identifier from projects.md |

## Prompts

### Architect: Debrief Interview

```
role: architect
phase: mission-debrief
project: {project-slug}
action: debrief-interview

Review the system changes produced across all flights in this mission. Examine
the architectural evolution, pattern consistency, and structural health.

Provide structured input for the debrief:

**Architectural Assessment**:
- Did the system's architecture improve, maintain, or degrade?
- Are there structural issues that emerged across flights?
- Were design decisions consistent across the mission?

**Pattern Analysis**:
- What patterns were established? Are they good ones?
- Is there inconsistency that should be reconciled?
- Are there reusable patterns worth documenting?

**Technical Debt**:
- What debt was introduced across the mission?
- What's the priority for addressing it?
- Are there quick wins vs. long-term concerns?

**Forward-Looking**:
- What architectural considerations should the next mission account for?
- Are there scaling or performance concerns on the horizon?
- What documentation or conventions should be established?
```
