# Legs

Legs are the AI-optimized layer of Flight Control. They provide structured, explicit instructions that AI agents can execute without ambiguity.

## What is a Leg?

A leg is a single, atomic unit of implementation work. Legs are:

- **Explicit**: No ambiguity about what "done" means
- **Bounded**: Clear start and end points
- **Context-complete**: All necessary information included
- **AI-consumable**: Structured for machine parsing

### Leg vs. Flight vs. Mission

| Aspect | Mission | Flight | Leg |
|--------|---------|--------|-----|
| Scope | Outcome | Feature | Task |
| Duration | Days-weeks | Hours-days | Minutes-hours |
| Modifications | Allowed | Allowed | Create new instead |
| Audience | Humans | Developers/AI | AI agents |

## Leg Structure

Legs follow a consistent structure optimized for AI consumption:

```markdown
# Leg: {slug}

## Flight Link
[Parent flight](../flight.md)

## Objective
Single sentence describing what this leg accomplishes.

## Context
Information the AI needs to understand this task.

## Inputs
- What exists before this leg runs

## Outputs
- What exists after this leg completes

## Acceptance Criteria
- [ ] Criterion 1
- [ ] Criterion 2
- [ ] Criterion 3

## Verification Steps
How to confirm each criterion is met (commands, manual checks, tools).

## Implementation Guidance
Specific patterns, approaches, or constraints.

## Files Likely Affected
- `path/to/file.ts`
- `path/to/test.ts`
```

## Writing Effective Legs

### Objectives

State exactly what the leg accomplishes in one sentence:

**Weak**:
> Set up the user stuff

**Strong**:
> Create the User model with email, password_hash, and timestamps fields

The objective should be:
- **Specific**: Clear what will be created/modified
- **Verifiable**: Can confirm completion by inspection
- **Atomic**: One cohesive piece of work

### Context

Provide information the AI needs but might not have:

```markdown
## Context

This project uses Prisma for database access. The existing `prisma/schema.prisma`
file contains the Post and Comment models. User will be referenced by these
models via foreign keys.

Authentication uses JWT tokens. The password_hash field will store bcrypt hashes
(cost factor 12). The email field must be unique and will be used as the login
identifier.
```

Good context includes:
- Relevant technology/framework information
- Relationship to existing code
- Design decisions from the parent flight
- Constraints that affect implementation

### Inputs and Outputs

Be explicit about state before and after:

```markdown
## Inputs
- Prisma schema at `prisma/schema.prisma` with Post and Comment models
- No existing User model or authentication

## Outputs
- User model added to Prisma schema
- Migration file generated
- Migration applied to development database
```

Inputs help the AI understand starting conditions. Outputs define the expected end state.

### Acceptance Criteria

Define exactly what "done" means:

```markdown
## Acceptance Criteria
- [ ] User model exists in `prisma/schema.prisma`
- [ ] User model has fields: id, email, password_hash, created_at, updated_at
- [ ] email field has `@unique` attribute
- [ ] Migration file exists in `prisma/migrations/`
- [ ] `npx prisma migrate status` shows no pending migrations
- [ ] TypeScript types generated (`npx prisma generate` succeeds)
```

Acceptance criteria should be:
- **Binary**: Either met or not met
- **Observable**: Can verify by inspection or test
- **Complete**: Nothing else required for "done"

### Verification Steps

Tell the AI exactly *how* to confirm each criterion:

```markdown
## Verification Steps
- Run `npx prisma migrate status` — should show no pending migrations
- Run `npm test` — all tests pass
- Open browser to `/users` — page loads without errors
- Tab through form fields — focus order matches visual order
- Run `npx lighthouse --accessibility` — score ≥ 90
```

Verification steps should be:
- **Executable**: Commands or specific actions
- **Deterministic**: Same result every time
- **Mapped to criteria**: Clear which criterion each step validates

For accessibility legs, include specific checks:
- Keyboard navigation sequences to test
- Screen reader commands (e.g., "navigate to main content via skip link")
- Automated tool commands (Lighthouse, axe-core)

### Implementation Guidance

Provide specific direction when needed:

```markdown
## Implementation Guidance

Use Prisma's native type for id:
```prisma
id String @id @default(cuid())
```

For timestamps, use Prisma's auto-managed fields:
```prisma
created_at DateTime @default(now())
updated_at DateTime @updatedAt
```

Do not add relations to Post/Comment yet—that's a separate leg.
```

Implementation guidance helps when:
- Project has specific patterns to follow
- There are multiple valid approaches (pick one)
- Constraints aren't obvious from context

### Files Likely Affected

Help the AI know where to look:

```markdown
## Files Likely Affected
- `prisma/schema.prisma` - Add User model
- `prisma/migrations/*` - New migration file (generated)
```

This isn't prescriptive—the AI might touch other files. It's a starting point for orientation.

## Leg Lifecycle

Legs progress through defined states:

### States

```
planning ──► ready ──► in-flight ──► landed ──► completed
                           │
                           └──► aborted
```

**planning**
Leg is being designed. Acceptance criteria and implementation guidance being defined.

**ready**
Leg design approved. Ready for implementation.

**in-flight**
AI agent actively working on implementation.

**landed**
Implementation complete. Flight log updated. Ready for review.

**completed**
Review passed. Acceptance criteria confirmed met.

**aborted**
Leg cancelled. Changes are rolled back. Document the reason in the flight log.

### State Transitions

| From | To | Trigger |
|------|----|---------|
| planning | ready | Design review passes |
| ready | in-flight | Developer begins work |
| in-flight | landed | Developer reports completion |
| in-flight | aborted | Cannot proceed, changes rolled back |
| landed | completed | Review passes |
| landed | in-flight | Issues found, needs fixes |

Note: Legs may only be modified while in `planning` state. Once `in-flight`, create new legs instead of modifying existing ones.

## Patterns for AI Consumption

### Be Explicit, Not Implicit

**Implicit** (requires inference):
> Add validation to the email field

**Explicit** (no inference needed):
> Add email validation: must be non-empty, valid email format (use validator library's isEmail), maximum 255 characters. Return 400 status with `{ error: "Invalid email format" }` on failure.

### Provide Examples

When patterns might be unclear, show don't tell:

```markdown
## Implementation Guidance

Follow the existing controller pattern:

```typescript
// Example from PostController
export async function createPost(req: Request, res: Response) {
  const { title, content } = req.body;

  if (!title) {
    return res.status(400).json({ error: "Title is required" });
  }

  const post = await prisma.post.create({
    data: { title, content, authorId: req.user.id }
  });

  return res.status(201).json(post);
}
```

Apply this pattern to the registration endpoint.
```

### State Constraints Clearly

Don't bury constraints in prose:

**Buried**:
> Create the endpoint and make sure it handles errors properly and validates input and also we're using Express and the response should be JSON.

**Clear**:
```markdown
## Constraints
- Framework: Express.js
- Response format: JSON
- Error handling: Return appropriate HTTP status codes
- Validation: Validate all input before processing
```

### Link to Flight for Context

When details would be redundant, reference the parent:

```markdown
## Context

See [parent flight](../flight.md) for:
- Authentication approach (JWT tokens)
- Session duration decisions
- Error response format standards
```

## Common Pitfalls

### Too Large

If a leg takes more than a few hours, it's probably too big. Signs:
- Multiple independent pieces of functionality
- Would benefit from intermediate checkpoints
- Hard to write clear acceptance criteria

Split into smaller legs.

### Too Small

If a leg is trivial, it adds overhead without value. Signs:
- Single line change
- No meaningful acceptance criteria
- Part of a larger atomic operation

Combine with related work.

### Ambiguous Acceptance Criteria

If criteria require judgment, they're not criteria:

**Ambiguous**: "Code is clean and readable"
**Specific**: "Functions are under 50 lines, no eslint warnings"

**Ambiguous**: "Error handling is good"
**Specific**: "All async operations wrapped in try/catch, errors logged with context"

### Missing Context

AI agents don't have your mental model. Include:
- Why this approach (from flight decisions)
- How this fits with existing code
- What patterns to follow
- What to avoid

## Relationship to Flight

Legs are generated from flights. The flight provides:
- Technical approach
- Design decisions
- Overall context

Legs provide:
- Specific implementation steps
- Explicit acceptance criteria
- Focused scope

A flight might generate many legs:

```
Flight: User Registration Flow
├── Leg: create-user-model
├── Leg: registration-endpoint
├── Leg: email-validation
├── Leg: password-hashing
├── Leg: registration-tests
└── Leg: registration-docs
```

## Immutability Principle

Once a leg is `in-flight`, don't modify it. If requirements change:

1. Mark the current leg as aborted (changes rolled back)
2. Create a new leg with updated requirements
3. Reference the old leg for context

This preserves history and prevents confusion about what the AI was asked to do.

## Next Steps

- [Workflow](workflow.md) — See the complete mission → flight → leg flow
- [Flights](flights.md) — Understand where legs come from
