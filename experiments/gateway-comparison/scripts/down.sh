#!/usr/bin/env bash
# Stop the comparison infrastructure for a profile. NON-DESTRUCTIVE:
# stops/removes containers only, NEVER volumes or data (no `-v`).
#
# Usage: ./down.sh <deterministic|real>
set -euo pipefail
cd "$(dirname "$0")/.."
PROFILE="${1:?usage: down.sh <deterministic|real>}"
FILE="docker-compose.${PROFILE}.yml"
# --remove-orphans is safe (containers only); volumes are never touched.
docker compose -f "$FILE" --profile melis --profile litellm down --remove-orphans
echo "stopped $PROFILE containers (no volumes/data removed)."
