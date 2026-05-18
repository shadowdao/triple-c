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

5. **State-machine reachability audit**

   For every state, status, or lifecycle value this leg introduces or relies on, verify nothing in a lower layer makes the state unreachable.

   - Enumerate the infrastructure layers that could foreclose the state: DB constraints (FK ON DELETE behaviors, NOT NULL, CHECK constraints, triggers), application caches and their invalidation rules, API/protocol version compatibility, fallback handlers that silently mask the state, and indexes that imply assumptions.
   - Audit existing tests for assertions that contradict the new design. A test that pins behavior the new state machine breaks must be inverted, renamed, or deleted as part of this leg — not after. The rename pattern (e.g., `test_X_does_Y` → `test_X_does_not_do_Y` with the assertion inverted) is preferred over delete-and-readd because it documents the intent shift in git blame.
   - When a design requires a row to persist past a referenced entity's deletion, explicitly inspect the schema's FK behaviors and any tests that pin them. A `ON DELETE CASCADE` on a referencing column will silently delete the row your design needs to keep.

6. **Cache freshness contract**

   For every cache (in-memory dict, query result cache, computed derived state, frontend session storage) this leg reads from or populates, declare an explicit freshness contract.

   - **Source of truth**: which underlying data does this cache reflect?
   - **Rebuild trigger**: pick exactly one — per-call rebuild, TTL with value, invalidation event, or accepted permanent staleness until process restart.
   - **Maximum staleness acceptable to the user**: be specific. "Until process restart" is rarely acceptable for user-facing state where users expect their edits to apply.
   - **User-action invalidation map**: list every user action that mutates the source-of-truth, and confirm each one invalidates or refreshes the cache. If a user can edit X in one UI surface but the cached X never sees the edit elsewhere, that mismatch will surface as a bug — name it now.

   Conflating "the cached object works fine" with "the cached object reflects current config" is a common error. State health and state freshness are different contracts.

7. **Identify dependent code** (for interface changes)
   - Does this leg modify shared interfaces?
   - What files consume these interfaces? Run `grep -rn '<changed_symbol>' tests/ src/`; if any test or non-leg-scope source imports or calls it, signature changes break any "prior tests pass UNMODIFIED" acceptance criterion.
   - Should updating consumers be part of this leg?

8. **Identify platform considerations**
   - Does this leg touch OS-specific features?
   - What platform differences might affect implementation?

### Phase 3: Guidance Generation

Create the leg artifact using the format defined in `.flightops/ARTIFACTS.md`.

### Phase 3b: Citation Verification

Before marking the leg `ready`, mechanically validate every code-location citation in the draft artifact against current code. Citations drift between when a flight is designed and when its legs are designed (intervening legs commit changes; source artifacts age). Catching drift here prevents the implementing agent from chasing a stale `file:line` into the wrong code.

1. **Extract citations**
   - Scan the leg artifact for code-location references matching `path/to/file.ext:line` or `path/to/file.ext:line-line` patterns
   - Also collect symbol-form citations: `path/to/file.ext:symbol_name`
   - Skip references to non-source artifacts (`mission.md:42`, `flight.md:100`) — those are out of scope

2. **Verify each citation**
   - Read the cited location in current code
   - Compare against the snippet, symbol, or surrounding description provided in the leg
   - Classify each:
     - **`OK`** — content at the cited location matches the description
     - **`drifted`** — content moved; the description is still accurate but the line number is wrong
     - **`gone`** — described content no longer exists in the file (or the file itself is gone)
     - **`unverifiable`** — citation has no snippet/symbol and the description is too vague to confirm

3. **Repair drift inline**
   - For `drifted`: locate the new line number via grep on the snippet/symbol and update the citation in the leg artifact
   - For `gone`: do not silently retire — flag for human review (the gap may have been independently fixed, OR the leg may now be obsolete)
   - For `unverifiable`: rewrite the citation using one of the durable forms (see "Citing Code Locations" guideline)

4. **Append a Citation Audit summary**
   - At the bottom of the leg artifact, append a clearly-titled section (something like `## Citation Audit` if the project's leg conventions don't dictate otherwise) summarizing the audit
   - If all citations verified clean: one sentence — `N citations verified against current code at leg design time.`
   - If any drift was repaired or flagged: list each one with classification and resolution

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

### Citing Code Locations

When the leg artifact references specific code, prefer durable forms over bare line numbers. Line numbers drift; symbols and snippets do not.

| Form | Example | When to use |
|------|---------|-------------|
| `file:symbol` | `the_one/api.py:create_provider` | Most cases — symbol names survive line shifts |
| `file:line — "snippet"` | `the_one/api.py:805 — "raise ProviderConfigError"` | When a specific line matters; the snippet is a self-verifier |
| `file:CONSTANT_NAME` | `web/middleware.py:GATED_METHODS` | Module-level constants and assignments |
| `file:line` (bare) | `api.py:805` | **Avoid** — brittle, no way to verify drift |

The snippet form is especially valuable: it lets Phase 3b mechanically confirm the cited content didn't move, and it tells the implementing agent exactly what they're looking at without needing to chase the line number.

When in doubt, include both — `the_one/api.py:805 (in create_provider) — "raise ProviderConfigError if base_url is empty"` — symbol + line + snippet covers all three drift modes.

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
