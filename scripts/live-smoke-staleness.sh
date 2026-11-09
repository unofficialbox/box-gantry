#!/usr/bin/env bash
#
# Catch a weekly live smoke that has quietly stopped running. The `alert` job
# in each live-smoke workflow reports a run that fails, but not a run that
# never happens (GitHub may drop a scheduled run, and does delay them by
# hours). So for each live-smoke workflow, find its newest passing run on
# main; when there is none within MAX_DAYS (default 8: the weekly schedule
# plus a day of slack), raise it through live-smoke-alert.sh as `stale`, which
# opens the same per-workflow issue a failure would. The next passing run
# closes it.
#
# Usage (the daily upstream-specs workflow runs it):
#   scripts/live-smoke-staleness.sh [--dry-run]

set -euo pipefail

REPO=${REPO:-unofficialbox/box-gantry}
MAX_DAYS=${MAX_DAYS:-8}
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
now=$(date -u +%s)

for file in livesmoke.yml apex-livesmoke.yml; do
  name=$(gh api "repos/$REPO/actions/workflows/$file" --jq .name)
  read -r id created <<<"$(gh api "repos/$REPO/actions/workflows/$file/runs?branch=main&status=success&per_page=1" \
    --jq '.workflow_runs[0] | "\(.id // "none") \(.created_at // "")"')"
  age=never
  if [ "$id" != none ]; then
    age=$(((now - $(date -u -d "$created" +%s)) / 86400))
    if [ "$age" -lt "$MAX_DAYS" ]; then
      echo "$name: last passed on main $age day(s) ago (run $id)"
      continue
    fi
  fi
  echo "$name: no passing run on main in the last $MAX_DAYS days (last: $age)"
  STALE_DAYS=$age "$here/live-smoke-alert.sh" "$name" "$id" stale "$@"
done
