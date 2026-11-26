#!/usr/bin/env bash
# Turns the end of the most recent step logs into error annotations, so a
# failure can be read from the public checks API without downloading logs.
set -u
dir=${1:?log directory}
for f in $(ls -t "$dir"/*.log 2>/dev/null | head -n 3); do
  msg=$(tail -n 120 "$f" | sed -e 's/%/%25/g' -e 's/\r/%0D/g' | awk '{printf "%s%%0A", $0}')
  echo "::error title=$(basename "$f")::$msg"
done
