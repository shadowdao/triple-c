---
name: leg
description: Generate detailed implementation guidance for LLM execution. Use when creating atomic implementation steps from a flight.
---

# Leg Implementation Guidance

Generate detailed implementation guidance for LLM execution.

## Prerequisites

- Project must be initialized with `/init-project` (`.flightops/ARTIFACTS.md` must exist)
- A flight must exist before creating legs

## Workflow

### Phase 1: Context Loading

1. **Identify the target project**
   - Read `projects.md` to find the project's path

2. **Verify project is initialized**
   - Check if `{target-project}/.flightops/ARTIFACTS.md` exists
   - **If missing**: STOP and tell the user to run `/init-project` first
   - Do not proceed without the artifact configuration

3. **Read the artifact configuration**
   - Read `{target-project}/.flightops/ARTIFACTS.md` for artifact locations and formats

4. **Read the parent flight**
   - Understand the objective being achieved
   - Review design decisions and constraints
   - Note the technical approach defined

5. **Read the flight log in detail** (critical)

   The flight log captures ground truth from actual implementation. Read it fully and extract:
   - Actual outcomes from completed legs
   - Deviations from the original plan
   - Anomalies discovered during execution
   - Environment details (versions, configurations)
   - Decisions made during implementation
   - Workarounds for issues encountered

6. **Identify this leg's scope**
   - Which leg from the flight's leg list?
   - What comes before and after?
   - Dependencies on other legs?
   - How do prior leg outcomes affect this leg?

7. **Identify environment constraints**
   - Execution environment (devcontainer, WSL, cloud)?
   - User context (root, specific user)?
   - Environment variables or shell setup needed?
   - Commands inside vs outside containers?

### Phase 2: Implementation Analysis

Deep dive into the specific implementation:

1. **Identify exact files to modify**
   - Read existing files that will be changed
   - Understand current code structure
   - Note imports, dependencies, patterns

2. **Understand existing patterns**
   - How is similar functionality implemented?
   - What conventions does the codebase follow?
   - What testing patterns are used?

3. **Determine inputs and outputs**
   - What state exists before this leg?
   - What state must exist after completion?
   - What can the implementing agent assume?

4. **Identify edge cases**
   - What could go wrong?
   - What validation is needed?
   - What error handling is required?
   - If this leg modifies database schemas: does it include migration creation AND execution? Both must happen in the same leg — a schema defined but never migrated is a gap.

5. **Identify dependent code** (for interface changes)
   - Does this leg modify shared interfaces?
   - What files consume these interfaces?
   - Should updating consumers be part of this leg?

6. **Identify platform considerations**
   - Does this leg touch OS-specific features?
   - What platform differences might affect implementation?

### Phase 3: Guidance Generation

Create the leg artifact using the format defined in `.flightops/ARTIFACTS.md`.

## Guidelines

### Writing Effective Objectives

State exactly what the leg accomplishes:

**Weak**: "Set up the database stuff"

**Strong**: "Create the User model with email, password_hash, and timestamp fields"

### Acceptance Criteria

Criteria must be:
- **Binary**: Either met or not met
- **Observable**: Can be verified by inspection or test
- **Complete**: Nothing else needed for "done"

**Weak**: "Code is clean" (subjective)

**Strong**: "User model exists in `prisma/schema.prisma`"

### Verification Steps

Tell the agent exactly *how* to confirm each criterion:

**Weak**: "Make sure it works"

**Strong**: 
```markdown
## Verification Steps
- Run `npx prisma migrate status` — should show no pending migrations
- Run `npm test` — all tests pass
- Tab through form fields — focus order matches visual order
```

For accessibility work, include specific checks:
- Keyboard navigation sequences
- Screen reader commands to test
- Automated tool commands (Lighthouse, axe-core)

### Implementation Guidance

Be explicit, not implicit:

**Implicit**: "Add validation to the email field"

**Explicit**: "Add email validation using the `validator` library's `isEmail` function. Return HTTP 400 with `{ "error": "Invalid email format" }` on validation failure."

### Code Examples

Provide examples when:
- The codebase has specific patterns to follow
- There are multiple valid approaches
- The implementation isn't obvious from context

### Leg Sizing

A well-sized leg:
- Takes minutes to a few hours
- Is atomic (can be completed independently)
- Has clear, verifiable acceptance criteria
- Produces a working increment

**Too small**: Single-line change with no meaningful criteria
**Too large**: Would benefit from intermediate checkpoints

### Documenting Workarounds

When implementing a workaround, document:
- **What**: The workaround clearly
- **Why**: Why the ideal solution wasn't feasible
- **When to remove**: Condition for replacement

### Immutability

Once a leg is `in-flight`:
- Do NOT modify the leg document
- If requirements change, mark it `aborted` (changes rolled back)
- Create a new leg with updated requirements

## Output

Create the leg artifact using the location and format defined in `.flightops/ARTIFACTS.md`.
