#!/usr/bin/env bash
# Smoke test ONE path through the common harness. Records artifact metadata.
# This is a smoke/pilot check of the plumbing, NOT a final measurement.
#
# Usage:
#   ./smoke.sh direct   deterministic json
#   ./smoke.sh melis     deterministic sse
#   ./smoke.sh litellm   deterministic json
#   ./smoke.sh melis     real          json   (model from --model below)
set -euo pipefail
cd "$(dirname "$0")/.."

PATH_NAME="${1:?direct|melis|litellm}"
PROFILE="${2:?deterministic|real}"
MODE="${3:-json}"
N="${4:-20}"

if [[ "$PROFILE" == "deterministic" ]]; then
  MODEL="mock-model"
  COUNT="http://127.0.0.1:7070/__count"
else
  MODEL="llama3.2"
  COUNT=""   # no backend-side counter for the real provider
fi

case "$PATH_NAME" in
  direct)  URL="http://127.0.0.1:7070"; [[ "$PROFILE" == real ]] && URL="http://127.0.0.1:11434" ;;
  melis)   URL="http://127.0.0.1:7071" ;;
  litellm) URL="http://127.0.0.1:4000" ;;
  *) echo "unknown path $PATH_NAME" >&2; exit 1 ;;
esac

META="$(mktemp)"
cat > "$META" <<JSON
{
  "melis_commit": "$(git -C ../.. rev-parse HEAD)",
  "melis_dirty": $(if [[ -n "$(git -C ../.. status --porcelain)" ]]; then echo true; else echo false; fi),
  "profile": "$PROFILE",
  "litellm_image": "ghcr.io/berriai/litellm@sha256:d781cd3d736b1bd34f9964ebc491a9f91fb8fa140f35d4e6b4176c19dd2b306b",
  "backend_image": "python@sha256:b18b261e1b4974b4f321451ad5251ded85abb22c2b8b262da527dda1d031ab30",
  "generated_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON

ARGS=(--path "$PATH_NAME" --url "$URL" --model "$MODEL" --mode "$MODE" --n "$N" --label smoke --meta "$META")
[[ -n "$COUNT" ]] && ARGS+=(--count-url "$COUNT")

python3 harness/bench_client.py "${ARGS[@]}"
rm -f "$META"
