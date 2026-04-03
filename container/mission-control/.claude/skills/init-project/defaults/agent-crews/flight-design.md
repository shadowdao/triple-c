# Flight Design — Project Crew

Crew definitions for flight specification. The Flight Director designs the
technical spec and uses project-side agents to validate against the real codebase.

## Crew

### Architect
- **Context**: {project}/
- **Model**: Sonnet
- **Role**: Reviews flight specs for technical soundness. Validates design
  decisions, prerequisites, technical approach, and leg breakdown against
  architecture best practices and actual codebase state. Ensures the flight
  is buildable and well-structured.
- **Actions**: review-flight-design

## Interaction Protocol

### Design Review
1. Flight Director creates flight spec and interviews human
2. Flight Director spawns **Architect** to review against codebase
3. Architect evaluates design decisions, prerequisites, approach, leg breakdown
4. Flight Director incorporates feedback
5. Max 2 review cycles — escalate to human if unresolved

## Template Variables

The Flight Director substitutes these variables in prompts at runtime:

| Variable | Description |
|----------|-------------|
| `{project-slug}` | Project identifier from projects.md |
| `{flight-number}` | Current flight number |
| `{flight-artifact-path}` | Path to the flight artifact file |

## Prompts

### Architect: Review Flight Design

```
role: architect
phase: flight-design-review
project: {project-slug}
flight: {flight-number}
action: review-flight-design

Read the flight artifact at {flight-artifact-path}. Cross-reference its design
decisions, prerequisites, technical approach, and leg breakdown against the actual
codebase state and architecture best practices.

Evaluate:
1. Design decisions — are they sound given the real codebase and architecture?
2. Prerequisites — are they accurate? Is anything missing or already done?
3. Technical approach — is it feasible? Does it follow existing patterns?
4. Leg breakdown — are legs well-scoped, properly ordered, with correct dependencies?
5. Codebase state — does the spec account for current working tree, existing tooling,
   and conventions that might affect implementation?
6. Architecture — does the approach maintain or improve system structure?

Provide structured output:

**Overall assessment**: approve | approve with changes | needs rework

**Issues** (ranked by severity):
- [high/medium/low] Description — recommended fix

**Suggestions** (non-blocking improvements):
- Description

**Questions** (for the designer to clarify):
- Question
```
