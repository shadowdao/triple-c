---
name: flight-debrief
description: Post-flight analysis for continuous improvement. Use after a flight is completed to capture lessons learned and improve the methodology.
---

# Flight Debrief

Perform comprehensive post-flight analysis for continuous improvement.

## Prerequisites

- Project must be initialized with `/init-project` (`.flightops/ARTIFACTS.md` must exist)
- A flight must have status `landed` before debriefing

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

4. **Load flight documentation**
   - Read the mission for overall context and success criteria
   - Read the flight for objectives, design decisions, and checkpoints
   - Read ALL legs to understand the planned implementation
   - Read the complete flight log for ground truth on what happened

5. **Load project context**
   - Read the target project's `README.md` and `CLAUDE.md`
   - Identify key implementation files from leg outputs and flight log

6. **Examine actual implementation**
   - Read files created or modified during the flight
   - Compare intended vs actual implementation
   - Note deviations, workarounds, or unexpected discoveries

### Phase 2: Crew Debrief Interviews

Read `{target-project}/.flightops/agent-crews/flight-debrief.md` for crew definitions and prompts (fall back to defaults at `.claude/skills/init-project/defaults/agent-crews/flight-debrief.md`).

**Validate structure**: The phase file MUST contain `## Crew`, `## Interaction Protocol`, and `## Prompts` sections with fenced code blocks. If the file exists but is malformed, STOP and tell the user: "Phase file `flight-debrief.md` is missing required sections. Either fix it manually or re-run `/init-project` to reset to defaults."

#### Developer Interview
1. **Spawn a Developer agent** in the target project context (Task tool, `subagent_type: "general-purpose"`)
   - Provide the "Debrief Interview" prompt from the flight-debrief phase file's Prompts section
   - The Developer examines code changes, test coverage, patterns, and technical debt
   - In addition to the crew prompt, the Flight Director directly instructs the Developer to: run the full test suite once, capture per-suite wall-clock timing, pass/fail counts (with names of any failing tests), skipped/ignored counts, and any flakes observed on the run; then read prior flight debriefs in the project and compare current metrics against any earlier numbers they find. Report findings as narrative — naming suites, quoting deltas, and proposing likely causes when visible from this flight's changes.
   - The Developer provides structured debrief input

#### Architect Interview
1. **Spawn an Architect agent** in the target project context (Task tool, `subagent_type: "general-purpose"`)
   - Provide the "Debrief Design Review" prompt from the flight-debrief phase file's Prompts section
   - The Architect compares planned design decisions against actual implementation
   - The Architect evaluates whether the flight design held up and provides feedback for future flights
   - This closes the design feedback loop — the same role that reviewed the spec now evaluates the outcome

#### Human Interview
Brief questions to capture insights documents may miss. Keep this lightweight — 2-3 questions max based on what you observed in the flight log.

- **On anomalies/deviations**: "The log mentions [X] — what drove that decision?"
- **On leg quality**: "Were any leg specs unclear or missing key context?"
- **On blockers**: "What slowed you down most? Was it predictable?"

Skip the human interview if the flight log is comprehensive and there are no obvious gaps.

### Phase 3: Deep Analysis

Synthesize Developer input, Architect input, human input, and document analysis across multiple dimensions:

#### Outcome Analysis
- Did the flight achieve its objective?
- Which mission success criteria did this flight advance?
- Were all checkpoints met?
- What value was delivered?

#### Process Analysis
- How accurate were the leg specifications?
- Were there gaps requiring improvisation?
- Did the leg sequence make sense?
- Were legs appropriately sized?
- Did acceptance criteria prove verifiable?

#### Technical Analysis
- What technical decisions were made during flight that weren't planned?
- Were there architectural surprises?
- What technical debt was introduced?
- Does implementation align with project conventions?

#### Test Metrics Capture
- Run the full test suite as part of the debrief and record metrics: per-suite wall-clock timing, pass/fail counts, skipped/ignored tests, and any flakes observed on this run.
- Read prior flight debriefs in this project for earlier metrics observations. Compare current numbers against whatever priors exist.
- Surface meaningful changes — significant slowdowns, new failures, growing skip lists, recurring flakes — as narrative observations in whichever existing section best fits (Technical, Key Learnings, or What Could Be Improved).
- If this is the first flight to capture metrics, the numbers seed comparisons for future flights.

#### Deviation Analysis
- What deviations occurred and why?
- Were deviations captured in the flight log?
- Should any deviations become standard practice?

#### Knowledge Capture
- What was learned that should be documented?
- Are there reusable patterns that emerged?
- Are README or CLAUDE.md updates needed?

### Phase 4: Skill Effectiveness Analysis

Evaluate whether the mission-control skills could be improved:

#### Mission Skill
- Did the mission provide adequate context?
- Were success criteria clear and measurable?

#### Flight Skill
- Did the flight structure support execution?
- Were design decisions adequately captured?
- Was the leg breakdown appropriate?

#### Leg Skill
- Did legs provide sufficient implementation guidance?
- Were acceptance criteria verifiable?
- Were edge cases adequately identified?

### Phase 5: Generate Debrief

Create the flight debrief artifact using the format defined in `.flightops/ARTIFACTS.md`.

### Phase 6: Flight Status Transition

Ask the user if the flight should be marked as `completed`. If confirmed, update the flight artifact's status from `landed` to `completed`.

## Guidelines

### Thoroughness Over Speed
- Read files completely, not just skim
- Consider root causes, not just symptoms
- Think about systemic improvements

### Be Specific and Actionable
Avoid vague recommendations. Instead of "improve documentation," say:
- "Add a 'Devcontainer Commands' section to CLAUDE.md documenting the docker exec workflow"

### Distinguish Severity
- **Critical**: Would have prevented significant rework or failure
- **Important**: Would have meaningfully improved efficiency
- **Minor**: Nice-to-have improvements

### Credit What Worked
Identify effective patterns that should be reinforced or codified.

### Consider the Meta-Level
- Did the mission/flight/leg hierarchy work?
- Were the right artifacts being created?
- Is there friction that could be eliminated?

## Output

Create the debrief artifact using the location and format defined in `.flightops/ARTIFACTS.md`.

After creating the debrief, summarize the top 3-5 most impactful recommendations.
