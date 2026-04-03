#!/bin/bash
# check-sync.sh - Compare .flightops directory between source and target
#
# Usage: check-sync.sh <source-dir> <target-dir>
#
# Outputs:
#   missing  - Target directory doesn't exist (neither .flightops/ nor .flight-ops/)
#   outdated - Target exists but one or more files differ from source
#   current  - All files match source
#
# Additional output lines:
#   agent-crews:{missing|empty|present}     - Crew directory status
#   crew-missing:{filename}                 - Default crew file not found in project (repeats per file)
#   legacy-layout:flight-ops                - .flight-ops/ detected (old name)
#   legacy-layout:phases                    - phases/ detected (old name)
#
# Note: Only checks synced files (README.md, FLIGHT_OPERATIONS.md).
#       ARTIFACTS.md and agent-crews/ are project-specific and not synced.

set -e

SOURCE_DIR="$1"
TARGET_DIR="$2"

if [[ -z "$SOURCE_DIR" || -z "$TARGET_DIR" ]]; then
  echo "Usage: check-sync.sh <source-dir> <target-dir>" >&2
  exit 1
fi

if [[ ! -d "$SOURCE_DIR" ]]; then
  echo "Error: Source directory not found: $SOURCE_DIR" >&2
  exit 1
fi

# Resolve the actual target directory, falling back to legacy name
EFFECTIVE_TARGET="$TARGET_DIR"
LEGACY_FLIGHT_OPS=false

if [[ ! -d "$TARGET_DIR" ]]; then
  # Derive the legacy path: replace trailing .flightops with .flight-ops
  LEGACY_DIR="${TARGET_DIR%/.flightops}/.flight-ops"
  if [[ "$LEGACY_DIR" != "$TARGET_DIR" && -d "$LEGACY_DIR" ]]; then
    EFFECTIVE_TARGET="$LEGACY_DIR"
    LEGACY_FLIGHT_OPS=true
  else
    echo "missing"
    exit 0
  fi
fi

# Compare each file in source directory
FILES_TO_CHECK=("FLIGHT_OPERATIONS.md" "README.md")
ALL_CURRENT=true

for FILE in "${FILES_TO_CHECK[@]}"; do
  SOURCE_FILE="$SOURCE_DIR/$FILE"
  TARGET_FILE="$EFFECTIVE_TARGET/$FILE"

  if [[ ! -f "$SOURCE_FILE" ]]; then
    continue
  fi

  if [[ ! -f "$TARGET_FILE" ]]; then
    ALL_CURRENT=false
    break
  fi

  SOURCE_HASH=$(sha256sum "$SOURCE_FILE" | cut -d' ' -f1)
  TARGET_HASH=$(sha256sum "$TARGET_FILE" | cut -d' ' -f1)

  if [[ "$SOURCE_HASH" != "$TARGET_HASH" ]]; then
    ALL_CURRENT=false
    break
  fi
done

if $ALL_CURRENT; then
  echo "current"
else
  echo "outdated"
fi

# Report on agent-crews directory existence, checking both current and legacy names
CREW_DIR=""
LEGACY_PHASES=false

if [[ -d "$EFFECTIVE_TARGET/agent-crews" ]]; then
  CREW_DIR="$EFFECTIVE_TARGET/agent-crews"
elif [[ -d "$EFFECTIVE_TARGET/phases" ]]; then
  CREW_DIR="$EFFECTIVE_TARGET/phases"
  LEGACY_PHASES=true
fi

if [[ -z "$CREW_DIR" ]]; then
  echo "agent-crews:missing"
elif [[ -z "$(ls -A "$CREW_DIR" 2>/dev/null)" ]]; then
  echo "agent-crews:empty"
else
  echo "agent-crews:present"
  # Check for missing crew files (new skills added since init)
  DEFAULT_CREWS_DIR="$SOURCE_DIR/defaults/agent-crews"
  if [[ -d "$DEFAULT_CREWS_DIR" ]]; then
    for DEFAULT_FILE in "$DEFAULT_CREWS_DIR"/*.md; do
      BASENAME=$(basename "$DEFAULT_FILE")
      if [[ ! -f "$CREW_DIR/$BASENAME" ]]; then
        echo "crew-missing:$BASENAME"
      fi
    done
  fi
fi

# Report legacy layout detection
if $LEGACY_FLIGHT_OPS; then
  echo "legacy-layout:flight-ops"
fi

if $LEGACY_PHASES; then
  echo "legacy-layout:phases"
fi
