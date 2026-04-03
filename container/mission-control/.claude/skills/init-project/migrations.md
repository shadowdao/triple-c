# Init-Project Migrations

When `/init-project` runs, it checks for legacy directory layouts from earlier versions of Flight Control and offers to migrate them. Migrations are idempotent — they only apply when the old layout is detected and the new layout doesn't yet exist.

## Migration Registry

### 001 — Rename `.flight-ops/` to `.flightops/`

Early versions of Flight Control used `.flight-ops/` (with a hyphen). The current convention is `.flightops/` (no hyphen).

**Detection** (returns true if migration is needed):

```bash
[[ -d "{target-project}/.flight-ops" && ! -d "{target-project}/.flightops" ]]
```

**Actions:**

1. Rename the directory:
   ```bash
   mv "{target-project}/.flight-ops" "{target-project}/.flightops"
   ```
2. Update `.gitignore` if it references the old name:
   ```bash
   sed -i 's/\.flight-ops/\.flightops/g' "{target-project}/.gitignore"
   ```

**User message:**
> Renaming `.flight-ops/` → `.flightops/` (updated naming convention)

---

### 002 — Rename `phases/` to `agent-crews/`

Early versions stored crew definitions in `.flightops/phases/`. The current convention is `.flightops/agent-crews/`.

**Detection** (returns true if migration is needed):

```bash
[[ -d "{target-project}/.flightops/phases" && ! -d "{target-project}/.flightops/agent-crews" ]]
```

> **Note:** This runs after migration 001, so it checks the post-rename `.flightops/` path.

**Actions:**

1. Rename the subdirectory:
   ```bash
   mv "{target-project}/.flightops/phases" "{target-project}/.flightops/agent-crews"
   ```

**User message:**
> Renaming `phases/` → `agent-crews/` (updated naming convention)

---

### 003 — Update lifecycle states to unified model

Flight Control now uses a unified lifecycle for both flights and legs: `planning → ready → in-flight → landed → completed` (or `aborted`). This replaces the old divergent states:

- **Flights**: `diverted` → `aborted`; added `completed` after `landed`
- **Legs**: `queued` → `planning`; `review` → `landed`; `blocked` → `aborted`; added `ready` and `completed`

**Detection** (returns true if migration is needed):

```bash
grep -rql 'queued\|diverted\|blocked\|review.*completed' "{target-project}/.flightops/ARTIFACTS.md" 2>/dev/null
```

> **Note:** This runs after migrations 001 and 002, so it checks the post-rename `.flightops/` path.

**Actions:**

1. Update state definitions in ARTIFACTS.md:
   - Replace flight status line: `planning | ready | in-flight | landed | diverted` → `planning | ready | in-flight | landed | completed | aborted`
   - Replace leg status line: `queued | in-flight | review | completed | blocked` → `planning | ready | in-flight | landed | completed | aborted`
   - Replace flight state tracking: `planning` → `ready` → `in-flight` → `landed` (or `diverted`) → `planning` → `ready` → `in-flight` → `landed` → `completed` (or `aborted`)
   - Replace leg state tracking: `queued` → `in-flight` → `review` → `completed` (or `blocked`) → `planning` → `ready` → `in-flight` → `landed` → `completed` (or `aborted`)
   - Replace `landed | diverted` → `landed | aborted` in debrief templates
   - Replace `landed/diverted` → `landed/aborted` in debrief templates
   - Replace `completed | in-flight | blocked` → `completed | landed | in-flight | aborted` in flight log templates

2. Update existing artifact files in the project (if any):
   - In flight artifacts: replace `**Status**: diverted` → `**Status**: aborted`
   - In leg artifacts: replace `**Status**: queued` → `**Status**: planning`, `**Status**: review` → `**Status**: landed`, `**Status**: blocked` → `**Status**: aborted`
   - In flight log entries: replace `**Status**: blocked` → `**Status**: aborted`

   Find artifacts using the locations defined in ARTIFACTS.md (typically `missions/` directory for file-based projects).

**User message:**
> Updating lifecycle states to unified model: flights and legs now share `planning → ready → in-flight → landed → completed (or aborted)`

---

## Adding Future Migrations

To add a new migration:

1. Assign the next sequential ID (e.g., `003`)
2. Write a **Detection** check that returns true only when the migration is needed and false if already applied (idempotent)
3. List the **Actions** to perform — prefer `mv` over copy-and-delete to preserve file contents and git history
4. Write a short **User message** shown during the migration summary
5. Consider ordering — if the migration depends on a previous one having run, note that in the detection section
6. Keep migrations non-destructive: rename and update references, never delete user content
