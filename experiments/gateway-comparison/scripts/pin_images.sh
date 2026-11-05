#!/usr/bin/env bash
# Resolve and print the pinned digests of the external images used here, so the
# compose files can reference @sha256:... instead of mutable tags. Run this before
# final runs and update the compose files if a digest changed.
set -euo pipefail
for ref in \
  "ghcr.io/berriai/litellm:main-v1.83.10-stable" \
  "python:3.12-slim" \
  "redis:7-alpine"; do
  echo "== $ref =="
  docker pull -q "$ref" >/dev/null
  docker image inspect "$ref" --format '{{index .RepoDigests 0}}'
done
