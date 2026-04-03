---
name: mission-debrief
description: Post-mission retrospective for outcomes assessment and methodology improvement. Use after a mission completes or aborts to capture overall lessons learned.
---

# Mission Debrief

Perform comprehensive post-mission retrospective and methodology assessment.

## Prerequisites

- Project must be initialized with `/init-project` (`.flightops/ARTIFACTS.md` must exist)
- A mission must have at least one completed or aborted flight before debriefing

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

4. **Load mission documentation**
   - Read the mission for original outcome, success criteria, and constraints
   - Read ALL flight documents for objectives and results
   - Read ALL flight debriefs for per-flight lessons learned
   - Read flight logs for execution details

5. **Load project context**
   - Read the target project's `README.md` and `CLAUDE.md`
   - Understand what was built during this mission

### Phase 2: Crew Debrief Interviews

Read `{target-project}/.flightops/agent-crews/mission-debrief.md` for crew definitions and prompts (fall back to defaults at `.claude/skills/init-project/defaults/agent-crews/mission-debrief.md`).

**Validate structure**: The phase file MUST contain `## Crew`, `## Interaction Protocol`, and `## Prompts` sections with fenced code blocks. If the file exists but is malformed, STOP and tell the user: "Phase file `mission-debrief.md` is missing required sections. Either fix it manually or re-run `/init-project` to reset to defaults."

#### Architect Interview
1. **Spawn an Architect agent** in the target project context (Task tool, `subagent_type: "general-purpose"`)
   - Provide the "Debrief Interview" prompt from the mission-debrief phase file's Prompts section
   - The Architect reviews architectural evolution across all flights, pattern consistency, and structural health
   - The Architect provides structured debrief input

#### Human Interview
Interview the crew to capture qualitative insights that documents alone cannot reveal.

##### Flight Log Clarifications
Surface specific observations from flight logs and ask for context:
- Anomalies or deviations noted in logs — what caused them?
- Decisions made during execution — what drove those choices?
- Blockers or delays — were these predictable in hindsight?
- Workarounds implemented — should these become standard practice?

##### Mission Control Experience
For the human(s) who served as mission control:
- "What was your experience coordinating this mission?"
- "Were there moments of confusion or uncertainty about status?"
- "Did the flight/leg structure help or hinder your oversight?"
- "What information was missing when you needed it?"

##### Project-Specific Feedback
- "What surprised you most during this mission?"
- "What would you do differently if starting over?"
- "Are there project-specific conventions that should be documented?"
- "Did any tools, libraries, or patterns prove particularly valuable or problematic?"

##### Agentic Orchestration Feedback (if applicable)
If the mission used automated orchestration (LLM agents executing legs):
- "How well did handoffs between agents work?"
- "Were there failures in agent coordination or context transfer?"
- "Did agents make decisions that required human correction?"
- "What guardrails or checkpoints would have helped?"
- "Was the level of autonomy appropriate for the tasks?"

**Note**: Adapt questions based on what the flight logs and artifacts reveal. Surface specific examples rather than asking in the abstract.

### Phase 3: Outcome Assessment (synthesize Architect + human input)

#### Success Criteria Evaluation
For each success criterion:
- Was it met? Partially met? Not met?
- What evidence supports this assessment?
- If not met, what blocked it?

#### Overall Outcome
- Did the mission achieve its stated outcome?
- Was the outcome still the right goal by the end?
- What value was delivered to stakeholders?

### Phase 4: Flight Analysis

#### Flight Summary
For each flight:
- Status (landed/completed/aborted)
- Key accomplishments
- Major challenges

#### Flight Patterns
- Which flights went smoothly? Why?
- Which flights struggled? Why?
- Were there common issues across flights?

### Phase 5: Process Analysis

#### Planning Effectiveness
- Was the initial flight plan accurate?
- How many flights were added/removed/changed?
- Were estimates reasonable?

#### Execution Patterns
- What worked well in execution?
- What friction points emerged?
- Were the right artifacts being created?

#### Methodology Assessment
- Did the mission/flight/leg hierarchy work for this project?
- Were briefings and debriefs valuable?
- What would you change about the process?

### Phase 6: Knowledge Capture

#### Lessons Learned
- Technical lessons (architecture, patterns, tools)
- Process lessons (planning, execution, communication)
- Domain lessons (business logic, requirements)

#### Reusable Patterns
- What patterns emerged that could be templated?
- What conventions should be documented?

#### Documentation Updates
- Does CLAUDE.md need updates?
- Does README need updates?
- Are there new runbooks or guides needed?

### Phase 7: Generate Debrief

Create the mission debrief artifact using the format defined in `.flightops/ARTIFACTS.md`.

### Phase 8: Mission Status Transition

If the mission is not already marked as `completed` or `aborted`, update the mission artifact's status to `completed`.

## Guidelines

### Holistic View
Look at the mission as a whole, not just individual flights. Identify patterns and systemic issues.

### Stakeholder Perspective
Frame outcomes in terms stakeholders care about. Did we deliver what was promised?

### Honest Assessment
Be candid about what didn't work. The debrief is for learning, not for blame.

### Actionable Insights
Every lesson should have a "so what?" — how should future missions be different?

### Methodology Feedback
This is the best time to identify improvements to Flight Control itself.

### Interview Integration
Weave interview insights throughout the debrief, not as a separate section. Crew perspectives should inform:
- Why certain outcomes were achieved or missed
- Root causes behind process friction
- Context that flight logs alone cannot capture
- Recommendations that reflect lived experience, not just document analysis

## Output

Create the debrief artifact using the location and format defined in `.flightops/ARTIFACTS.md`.

After creating the debrief, summarize:
1. Overall mission outcome assessment
2. Top 3 things that went well
3. Top 3 things to improve
4. Recommended methodology changes
