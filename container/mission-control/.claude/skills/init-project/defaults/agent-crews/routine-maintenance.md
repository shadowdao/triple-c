# Routine Maintenance — Project Crew

Crew definitions for codebase health inspection. The Flight Director
coordinates specialist reviewers for automated checks and an Architect
for severity assessment and roundtable moderation.

## Crew

### Inspector
- **Context**: {target-project}/
- **Model**: Sonnet
- **Role**: Performs broad read-only codebase inspection across all applicable
  categories. Runs test suites, linters, type checkers, audit commands, and
  manual code review. Returns structured findings without modifying any files.
- **Actions**: inspect-codebase

### Security Reviewer
- **Context**: {target-project}/
- **Model**: Sonnet
- **Role**: Performs focused manual security review of authentication flows,
  injection surfaces, secrets handling, CORS/CSP configuration, and data
  exposure risks. Goes deeper than the Inspector's Category 1 automated checks
  with targeted code path analysis.
- **Actions**: review-security

### CI/CD Reviewer (optional)
- **Context**: {target-project}/
- **Model**: Sonnet
- **Enabled**: false (enable when project has CI/CD pipelines)
- **Role**: Reviews CI/CD pipeline configuration, build security, deployment
  practices, and environment consistency. Evaluates pipeline definitions,
  secret management in CI, and deployment safeguards.
- **Actions**: review-cicd

### Accessibility Reviewer (optional)
- **Context**: {target-project}/
- **Model**: Sonnet
- **Enabled**: false (enable when project has user-facing UI)
- **Role**: Reviews codebase for accessibility compliance against WCAG 2.1 AA
  standards. Evaluates semantic HTML, keyboard navigation, screen reader
  compatibility, color contrast, ARIA usage, and focus management.
- **Actions**: review-accessibility

### Architect
- **Context**: {target-project}/
- **Model**: Opus
- **Role**: Reviews all reviewer findings alongside debrief context. Assigns
  severity per finding, challenges questionable assessments, moderates
  roundtable discussion with specialist reviewers, and produces final codebase
  assessment with maintenance scope recommendation.
- **Actions**: assess-findings, moderate-roundtable

## Separation Rules

- All reviewers are strictly **read-only** — they may run commands but must NEVER modify files
- Each reviewer operates independently during Phase 4 — no cross-reviewer communication
- The Architect sees all reviewer findings but not their internal reasoning
- Roundtable discussion is mediated by the Flight Director, not direct agent-to-agent

**Note:** Handoff signals are not used in this crew. The routine-maintenance workflow is
sequential (review → assess → roundtable → report) and does not use the leg-based
handoff protocol.

## Interaction Protocol

### Delegation Planning
1. Flight Director loads context, conducts scoping interview with human
2. Flight Director assesses project size and identifies module boundaries
3. Flight Director builds delegation plan (agent count, scope assignments, partitioning)
4. Human approves or adjusts the plan

### Specialist Review
1. Flight Director spawns agents per the delegation plan — Inspector(s) + Security Reviewer always, CI/CD and Accessibility if enabled
2. Each agent receives its scope assignment and output discipline rules
3. All reviewers perform read-only checks and return structured findings
4. For partitioned Inspectors: Flight Director merges and de-duplicates findings

### Initial Assessment
1. Flight Director spawns **Architect** (Opus) with all reviewer findings + debrief context
2. Architect assigns initial severity per finding
3. Architect raises challenges or questions directed at specific reviewers

### Roundtable
1. Flight Director routes Architect's challenges to the relevant reviewers
2. Each challenged reviewer responds with evidence, rebuttals, or concurrence
3. Flight Director collects responses and spawns Architect for final resolution
4. Architect produces final assessment incorporating roundtable discussion
5. Max 2 roundtable cycles — unresolved disagreements go to the human

### Human Review and Scoping
1. Flight Director presents findings to human, grouped by severity
2. Human confirms, overrides, or adjusts findings
3. If Maintenance Required: Flight Director recommends a shortlist (~5-7 items); human selects scope for maintenance mission
4. Deferred findings remain in the report for future cycles

### Synthesis
1. Flight Director generates maintenance report artifact
2. If confirmed: Flight Director creates maintenance mission scaffold

## Template Variables

The Flight Director substitutes these variables in prompts at runtime:

| Variable | Description |
|----------|-------------|
| `{project-slug}` | Project identifier from projects.md |
| `{applicable-categories}` | Numbered list of categories to inspect (1-7 always, 8-10 conditional) |
| `{project-stack}` | Language, framework, test runner, linter, formatter, type checker, audit tool |
| `{known-debt}` | Debt items from mission debrief and flight debriefs (if available, otherwise "None — ad-hoc inspection") |
| `{known-security-debt}` | Security-specific debt items extracted from debriefs (if available, otherwise "None") |
| `{known-cicd-debt}` | CI/CD-specific debt items extracted from debriefs (if available, otherwise "None") |
| `{areas-of-concern}` | User-specified areas of concern from scoping interview |
| `{scope-assignment}` | Scope restriction from the delegation plan (files, directories, or "full project") |
| `{all-reviewer-findings}` | Combined structured findings from all reviewers (used in Architect prompts) |
| `{architect-challenges}` | Architect's challenges directed at a specific reviewer (used in roundtable) |
| `{roundtable-responses}` | All reviewer rebuttals and responses from the roundtable (used in resolution) |

## Prompts

### Inspector: Inspect Codebase

```
role: inspector
phase: routine-maintenance
project: {project-slug}
action: inspect-codebase

Perform a read-only codebase inspection across the following categories:
{applicable-categories}

Project stack: {project-stack}

Known debt from prior debriefs, if available (do not re-flag as new discoveries):
{known-debt}

User areas of concern:
{areas-of-concern}

IMPORTANT: You are strictly READ-ONLY. You may run test suites, linters, type
checkers, audit commands, and read any file. You must NEVER modify source files,
configuration, dependencies, or any other project file.

**Scope assignment**: If a scope restriction is provided, inspect only the
specified files and directories. Run automated tools against the full project
(tools are fast and comprehensive), but limit manual code review to the assigned
scope. If no scope restriction is given, inspect the full project.

For each applicable category, perform the checks listed below and report findings.

**Category 1 — Security**:
- Review auth paths (focus on recently changed code if mission context is available)
- Check input sanitization on endpoints
- Verify CORS/CSP configuration
- Scan for hardcoded secrets (API keys, tokens, passwords)
- Review third-party data flow for exposure risks

**Category 2 — Test Systems**:
- Run the test suite and report results
- Check coverage delta (if tooling available)
- Find new code paths without test coverage
- Detect flaky tests (tests that pass/fail inconsistently)
- Check test performance (slow tests)
- Find hardcoded test data that should be fixtures

**Category 3 — Dependency Health**:
- Run the dependency audit command (npm audit, cargo audit, etc.)
- Check for outdated dependencies
- Find unused dependencies
- Verify lockfile is consistent
- Check license compliance
- Check for Dependabot/Renovate PRs and security alerts
- Assess auto-merge eligibility for patch updates

**Category 4 — Code Quality**:
- Run linter and formatter check (report violations, do NOT fix)
- Find dead code (unused exports, unreachable branches)
- Grep for TODOs/FIXMEs/HACKs (focus on recently introduced ones if mission context is available)
- Detect code duplication
- Check pattern consistency with existing codebase

**Category 5 — Type & API Safety**:
- Run the type checker and report errors
- Find `any` casts (TypeScript), `unsafe` blocks (Rust), or equivalent
- Check for unhandled errors or missing error types
- Detect API contract drift (mismatched types between client/server)
- Find deprecated API usage

**Category 6 — Documentation**:
- Check README accuracy against current state
- Verify new public interfaces have documentation
- Find stale comments referencing old behavior
- Check CHANGELOG for completeness
- Verify CLAUDE.md accuracy

**Category 7 — Git & Branch Hygiene**:
- List stale branches (merged but not deleted)
- Find large committed files (>1MB)
- Scan for secrets in recent git history
- Check commit message quality
- Check for GitHub/remote warnings (secret scanning, code scanning alerts)
- Find merge conflicts against main
- Check upstream divergence

**Category 8 — CI/CD Pipeline** (if applicable):
- Check CI status on main/default branch
- Detect build time regression
- Find skipped or disabled checks
- Check config drift between environments

**Category 9 — Infrastructure & Config** (if applicable):
- Check env var documentation (.env.example vs actual usage)
- Find pending database migrations
- Find temporary feature flags that should be removed

**Category 10 — Performance & Observability** (if applicable):
- Find new operations without logging/tracing
- Detect potential N+1 queries
- Check bundle size (if web project)
- Find resource cleanup issues (unclosed connections, missing cleanup)

**Output discipline**: Keep findings concise. Do not paste full command output,
full file contents, or long dependency lists. Summarize and reference.

**Output format**: Return findings as a structured list per category:

## Category {N}: {Name}

### Finding: {title}
- **Evidence**: {one-line summary with file paths and line numbers}
- **Impact**: {what could go wrong}
- **Recommendation**: {what to do about it}

Include code excerpts only for Critical or High severity findings.

If a category has no issues, report:
## Category {N}: {Name}
No issues found.
```

### Security Reviewer: Review Security

```
role: security-reviewer
phase: routine-maintenance
project: {project-slug}
action: review-security

Perform a focused, manual security review of the codebase. You go deeper than
automated scanning — trace actual code paths and evaluate security posture.

Project stack: {project-stack}

Known security debt from prior debriefs (do not re-flag as new discoveries):
{known-security-debt}

User areas of concern:
{areas-of-concern}

IMPORTANT: You are strictly READ-ONLY. You may run commands and read any file.
You must NEVER modify source files, configuration, dependencies, or any other
project file.

**Scope assignment**: Review only the files and areas specified. If no scope
restriction is given, review the full project.

**Output discipline**: Keep findings concise. Include code excerpts only for
Critical or High severity findings. Do not paste full file contents or raw
command output.

**Review areas**:

1. **Authentication & Authorization**
   - Trace auth flows end-to-end (login, token refresh, logout)
   - Check for missing auth checks on protected routes/endpoints
   - Verify role-based access control is enforced consistently
   - Look for privilege escalation paths

2. **Injection Surfaces**
   - SQL/NoSQL injection: check all database queries for parameterization
   - Command injection: check shell executions, subprocess calls
   - XSS: check output encoding in templates and API responses
   - Path traversal: check file system operations with user input

3. **Secrets & Configuration**
   - Scan for hardcoded credentials, API keys, tokens in source
   - Check .env files are gitignored
   - Verify secrets are not logged or included in error responses
   - Check for overly permissive CORS configuration

4. **Data Handling**
   - Review PII/sensitive data flows — where is it stored, logged, transmitted?
   - Check encryption at rest and in transit
   - Verify sensitive data is not cached inappropriately
   - Check for data leakage in error messages or debug output

5. **Dependency Risk**
   - Cross-reference critical dependencies against known CVE databases
   - Check for dependencies with known supply-chain risks
   - Verify integrity checks (lockfile hashes, checksums)

**Output format**: Return findings as a structured list:

### Finding: {title}
- **Severity estimate**: critical | high | medium | low
- **Attack vector**: {how this could be exploited}
- **Evidence**: {specific code paths, file:line references}
- **Recommendation**: {what to do about it}

If no security issues found, state: "No security issues identified."
```

### CI/CD Reviewer: Review CI/CD

```
role: cicd-reviewer
phase: routine-maintenance
project: {project-slug}
action: review-cicd

Perform a focused review of the project's CI/CD pipeline configuration,
build security, and deployment practices.

Project stack: {project-stack}

Known CI/CD debt from prior debriefs (do not re-flag as new discoveries):
{known-cicd-debt}

User areas of concern:
{areas-of-concern}

IMPORTANT: You are strictly READ-ONLY. You may run commands and read any file.
You must NEVER modify source files, configuration, dependencies, or any other
project file.

**Output discipline**: Keep findings concise. Include code excerpts only for
Critical or High severity findings. Do not paste full file contents or raw
command output.

**Review areas**:

1. **Pipeline Configuration**
   - Review pipeline definitions (GitHub Actions, GitLab CI, Concourse, etc.)
   - Check for outdated action/image versions
   - Verify branch protection rules are consistent with pipeline triggers
   - Detect redundant or overlapping pipeline steps

2. **Build Security**
   - Check for secrets exposed in build logs or artifacts
   - Verify pipeline secrets are scoped appropriately (not org-wide when repo-level suffices)
   - Check for unpinned dependencies in build steps (e.g., `uses: action@main` vs `@v4.1.0`)
   - Review build artifact permissions and retention policies

3. **Deployment Safeguards**
   - Verify deployment gates exist (approval steps, environment protection rules)
   - Check rollback capability — is there a documented or automated rollback path?
   - Verify environment promotion flow (dev → staging → prod) is enforced
   - Check for drift between environment configurations

4. **Pipeline Health**
   - Check recent build success rates and durations
   - Identify flaky pipeline steps
   - Find disabled or skipped checks that should be active
   - Check for resource waste (oversized runners, unnecessary matrix builds)

**Output format**: Return findings as a structured list:

### Finding: {title}
- **Severity estimate**: critical | high | medium | low
- **Evidence**: {specific config files, pipeline definitions, line references}
- **Impact**: {what could go wrong}
- **Recommendation**: {what to do about it}

If no CI/CD issues found, state: "No CI/CD issues identified."
```

### Accessibility Reviewer: Review Accessibility

```
role: accessibility-reviewer
phase: routine-maintenance
project: {project-slug}
action: review-accessibility

Perform a focused accessibility review of the project's user-facing UI.
Evaluate against WCAG 2.1 AA standards.

Project stack: {project-stack}

IMPORTANT: You are strictly READ-ONLY. You may run commands and read any file.
You must NEVER modify source files, configuration, dependencies, or any other
project file.

**Output discipline**: Keep findings concise. Include code excerpts only for
Critical or High severity findings. Do not paste full file contents or raw
command output.

**Review areas**:

1. **Semantic HTML & Structure**
   - Check heading hierarchy (h1-h6 in logical order)
   - Verify landmark regions (main, nav, aside, footer)
   - Check form labels and fieldset/legend usage
   - Verify list markup for list-like content

2. **Keyboard Navigation**
   - Check all interactive elements are reachable via Tab
   - Verify custom widgets have appropriate keyboard handlers
   - Check for keyboard traps (modals, dropdowns)
   - Verify skip-to-content links exist

3. **Screen Reader Compatibility**
   - Check ARIA attributes for correctness and necessity
   - Verify dynamic content updates use live regions
   - Check image alt text (present, meaningful, not redundant)
   - Verify form error messages are associated with inputs

4. **Visual & Color**
   - Check text contrast ratios (4.5:1 normal, 3:1 large text)
   - Verify UI component contrast (3:1 against background)
   - Check that color is not the sole indicator of meaning
   - Verify visible focus indicators on all interactive elements

5. **Motion & Timing**
   - Check for prefers-reduced-motion support on animations
   - Verify no auto-playing media without controls
   - Check for appropriate timeouts with user notification

**Output format**: Return findings as a structured list:

### Finding: {title}
- **WCAG criterion**: {e.g., 1.1.1 Non-text Content, Level A}
- **Severity estimate**: critical | high | medium | low
- **Evidence**: {specific components, file:line references}
- **Recommendation**: {what to do about it}

If no accessibility issues found, state: "No accessibility issues identified."
```

### Architect: Assess Findings

```
role: architect
phase: routine-maintenance
project: {project-slug}
action: assess-findings

Review all specialist findings and assign severity ratings. You have access to:
- All reviewer findings (provided below)
- Known debt context from debriefs and prior maintenance reports (if available)

{all-reviewer-findings}

Known debt from debriefs, if available (already acknowledged — note as "previously identified" if re-found):
{known-debt}

For each finding, assign one of:
- **Pass** — No issue (reviewer flagged something that is actually fine)
- **Advisory** — Minor issue, acceptable to defer
- **Action Required** — Should be addressed before next major work cycle
- **Critical** — Blocks further work, immediate attention needed

**Assessment criteria**:
- Does this finding represent a real risk, or is it noise?
- Is the severity proportional to the actual impact?
- Would this compound if left for another cycle?
- Is the infrastructure or framing this finding pertains to still serving its original purpose, or has it drifted into "maybe-someday" territory?
- Is this a new discovery or previously acknowledged debt?
- Do multiple reviewers corroborate the same issue?
- Are any reviewer assessments questionable — too alarmist or too dismissive?

**Challenge reviewers** where you disagree or need clarification. For each
challenge, name the reviewer and provide your specific question or objection.
This initiates the roundtable discussion.

**Output format**:

## Overall Assessment
{Flight Ready | Maintenance Required}

## Findings

| # | Source | Category | Finding | Initial Severity | New/Known | Notes |
|---|--------|----------|---------|-----------------|-----------|-------|
| 1 | {reviewer} | {cat} | {title} | {severity} | {new/known} | {brief note} |

## Challenges for Roundtable

### To {Reviewer Name}: {question or objection}
{Context for why you're challenging this finding — what seems off, what
additional evidence would change your assessment, or why you think the
severity should be different.}

## Severity Summary
- Critical: {N}
- Action Required: {N}
- Advisory: {N}
- Pass: {N}

## Recommended Maintenance Scope
(Only if Maintenance Required)

Group related Action Required and Critical findings into suggested flight scopes:

### Flight: {suggested title}
- Finding #{N}: {title}
- Finding #{N}: {title}
- Rationale: {why these group together}
```

### Reviewer: Roundtable Rebuttal

```
role: {reviewer-role}
phase: routine-maintenance
project: {project-slug}
action: roundtable-rebuttal

The Architect has challenged one or more of your findings during the
severity assessment roundtable. Respond to each challenge with evidence.

Architect's challenges:
{architect-challenges}

For each challenge:
1. **Provide additional evidence** — code paths, specific examples, tool output
   that supports your finding
2. **Concede if appropriate** — if the Architect raises a valid point, adjust
   your assessment rather than defending a weak position
3. **Clarify misunderstandings** — if the Architect misread your finding,
   restate it with more precision

Be direct and evidence-based. The goal is consensus, not debate for its own sake.

**Output format**:

### Re: {Architect's challenge title}
- **Response**: {concur | rebut | clarify}
- **Evidence**: {additional code paths, line references, tool output}
- **Revised assessment** (if changed): {updated severity or recommendation}
```

### Architect: Roundtable Resolution

```
role: architect
phase: routine-maintenance
project: {project-slug}
action: roundtable-resolution

Review the roundtable responses from specialist reviewers and produce your
final assessment.

Reviewer responses:
{roundtable-responses}

For each challenged finding:
1. **Weigh the evidence** — did the reviewer provide convincing support?
2. **Assign final severity** — this is your call, but account for reviewer expertise
3. **Note reasoning** — briefly explain why you maintained or changed severity

If any disagreements remain unresolved, flag them for human review rather than
forcing consensus.

**Output format**:

## Roundtable Resolution

### Finding #{N}: {title}
- **Original severity**: {severity}
- **Reviewer response**: {concur | rebut | clarify} — {summary}
- **Final severity**: {severity}
- **Reasoning**: {why}

## Updated Overall Assessment
{Flight Ready | Maintenance Required}

## Updated Severity Summary
- Critical: {N}
- Action Required: {N}
- Advisory: {N}
- Pass: {N}

## Unresolved Disagreements (if any)
{Finding and both perspectives — for human to decide}

## Updated Recommended Maintenance Scope
(Only if Maintenance Required — incorporate roundtable outcomes)

### Flight: {suggested title}
- Finding #{N}: {title}
- Finding #{N}: {title}
- Rationale: {why these group together}
```
