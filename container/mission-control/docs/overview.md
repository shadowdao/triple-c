# Flight Control Overview

Flight Control is a methodology for AI-first software development that maintains meaningful human oversight while maximizing AI effectiveness.

## Philosophy

### AI-First, Human-Guided

Traditional development methodologies were designed for human developers. They assume humans will interpret requirements, make design decisions, and adapt to changing circumstances. AI agents work differently—they excel with explicit structure but struggle with ambiguity.

Flight Control inverts the traditional approach:

- **Humans define outcomes**, not implementation details
- **AI executes implementations**, not strategic decisions
- **The methodology bridges the gap** through progressive specification

### Why Aviation Works

Aviation provides a proven model for high-stakes operations where planning and execution are separate concerns:

| Aviation | Flight Control |
|----------|----------------|
| Mission objectives | Mission outcomes |
| Flight plan | Flight specification |
| Flight legs | Implementation legs |
| Pilot authority | Human oversight |
| Autopilot execution | AI execution |

The key insight: pilots don't recompute routes in real-time. They follow pre-computed flight plans while retaining authority to adapt when circumstances demand. Similarly, AI agents shouldn't reinvent architecture with each task—they should execute well-specified legs while flagging issues for human review.

## Key Principles

### 1. Outcome-Driven Planning

Missions start with outcomes, not tasks:

**Traditional**: "Build a user authentication system"
**Flight Control**: "Users can securely access their accounts with minimal friction"

The outcome framing keeps focus on what matters while leaving implementation flexible.

### 2. Adaptive Specifications

Flights are living documents. Unlike traditional specs that become stale, flight plans explicitly track:

- Open questions requiring resolution
- Design decisions and their rationale
- Prerequisites and dependencies
- Adaptation criteria (when to deviate from plan)

### 3. Structured Execution

Legs are optimized for AI consumption:

- Explicit acceptance criteria
- Required context clearly stated
- Expected inputs and outputs defined
- No ambiguity in scope

### 4. Layered Feedback

Information flows both directions:

- **Downward**: Missions inform flights, flights generate legs
- **Upward**: Leg completion updates flights, flight outcomes inform mission status

## Comparison to Traditional Methodologies

### vs. Agile/Scrum

Agile emphasizes iterative human collaboration. Flight Control complements this by structuring how AI fits into iterations:

- Sprints can contain multiple flights
- Stories map roughly to flights
- Tasks map to legs

Flight Control adds the missing layer: how to specify work for AI execution.

### vs. Waterfall

Waterfall assumes complete upfront specification. Flight Control embraces uncertainty:

- Missions can spawn new flights as understanding evolves
- Flights can be modified in-flight when circumstances change
- Legs can be aborted and replaced

### vs. CRISP-DM / ML Workflows

Data science workflows focus on experimentation. Flight Control adds structure without eliminating iteration:

- Experimental flights can have "explore" legs
- Failed experiments inform mission outcomes
- Reproducibility is built into leg specifications

## The Audience Gradient

A core innovation is the **audience gradient**—documentation shifts style based on who consumes it:

```
Human Readable ◄─────────────────────────────► AI Optimized
     │                    │                         │
  Mission              Flight                      Leg
     │                    │                         │
 Narrative prose    Technical spec          Structured format
 Outcome-focused    Checklist-driven        Explicit criteria
 Flexible scope     Bounded scope           Fixed scope
```

This gradient acknowledges that humans and AI have different strengths:

- Humans excel at ambiguity, context, and strategic thinking
- AI excels at following explicit instructions consistently

Flight Control puts each audience where they're strongest.

## When to Use Flight Control

Flight Control works best when:

- AI agents are part of your development workflow
- Work benefits from clear specification before execution
- You need traceability from outcomes to implementation
- Multiple people (or AI sessions) contribute to a single outcome

It may be overkill for:

- Quick one-off scripts
- Solo exploratory coding
- Highly uncertain R&D with no clear outcomes

## Next Steps

- [Missions](missions.md) — Learn to write outcome-driven mission statements
- [Flights](flights.md) — Create technical specifications with pre/post checklists
- [Legs](legs.md) — Structure AI-optimized implementation steps
- [Workflow](workflow.md) — Understand the end-to-end flow
