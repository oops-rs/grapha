#!/usr/bin/env sh
# Grapha container entrypoint.
#
# Default behavior (turnkey): index the mounted workspace if it has no
# `.grapha` store yet, then launch the HTTP graph explorer. Override any
# step with the environment variables below, or pass an explicit grapha
# command as arguments to bypass the index+serve flow entirely.
#
#   GRAPHA_WORKSPACE  Directory to index and serve   (default: /workspace)
#   GRAPHA_HOST       Interface to bind              (default: 0.0.0.0)
#   GRAPHA_PORT       Port to listen on              (default: 8080)
#   GRAPHA_REINDEX    Re-index on start when set     (default: unset)
#
# Examples:
#   docker run -v "$PWD:/workspace" -p 8080:8080 grapha
#   docker run -v "$PWD:/workspace" grapha index /workspace --format json
#   docker run grapha --help
set -eu

WORKSPACE="${GRAPHA_WORKSPACE:-/workspace}"
HOST="${GRAPHA_HOST:-0.0.0.0}"
PORT="${GRAPHA_PORT:-8080}"

# Any explicit arguments are passed straight through to the grapha CLI.
if [ "$#" -gt 0 ]; then
  exec grapha "$@"
fi

if [ ! -d "$WORKSPACE" ]; then
  echo "grapha: workspace '$WORKSPACE' not found — mount a repo there, e.g. -v \"\$PWD:$WORKSPACE\"" >&2
  exit 1
fi

if [ -n "${GRAPHA_REINDEX:-}" ] || [ ! -d "$WORKSPACE/.grapha" ]; then
  echo "grapha: indexing $WORKSPACE ..."
  grapha index "$WORKSPACE"
else
  echo "grapha: reusing existing index at $WORKSPACE/.grapha (set GRAPHA_REINDEX=1 to rebuild)"
fi

echo "grapha: serving $WORKSPACE on $HOST:$PORT"
exec grapha serve -p "$WORKSPACE" --host "$HOST" --port "$PORT"
