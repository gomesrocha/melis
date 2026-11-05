#!/usr/bin/env bash
# Start the comparison infrastructure for one PROFILE and one GATEWAY at a time.
# Gateways are started separately (compose profiles) to avoid resource interference.
#
# Usage:
#   ./up.sh deterministic backend          # start the shared mock backend
#   ./up.sh deterministic melis            # start Melis (needs backend up)
#   ./up.sh deterministic litellm          # start LiteLLM (needs backend up)
#   ./up.sh real melis                     # real profile (host Ollama must be up)
#   ./up.sh real litellm
#
# NOTE: this NEVER removes volumes or data. It only starts containers.
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="${1:?usage: up.sh <deterministic|real> <backend|melis|litellm>}"
TARGET="${2:?usage: up.sh <deterministic|real> <backend|melis|litellm>}"
FILE="docker-compose.${PROFILE}.yml"

if [[ ! -f "$FILE" ]]; then echo "no compose file $FILE" >&2; exit 1; fi

case "$TARGET" in
  backend) docker compose -f "$FILE" up -d backend ;;
  melis)   docker compose -f "$FILE" --profile melis up -d --build melis ;;
  litellm) docker compose -f "$FILE" --profile litellm up -d litellm ;;
  *) echo "unknown target $TARGET" >&2; exit 1 ;;
esac

echo "started $TARGET ($PROFILE). Endpoints:"
echo "  direct  (deterministic): http://127.0.0.1:7070"
echo "  melis:                   http://127.0.0.1:7071"
echo "  litellm:                 http://127.0.0.1:4000"
