#!/usr/bin/env bash
#
# Keep one GitHub issue per live-smoke workflow in step with its latest run, so
# a failing weekly smoke (a Box API change, an expired credential, a runtime
# regression) is seen without anyone watching Actions:
#
#   failure  -> open a `live-smoke-failure` issue naming the failed jobs and
#               steps with a log excerpt, or comment on the open one
#   success  -> comment on the open issue, if any, and close it
#   other    -> (cancelled, skipped) nothing
#
# The issue is matched by a marker naming the workflow, so the Go/Rust/TS/Java
# smoke and the Apex smoke each keep their own.
#
# Usage (the live-smoke workflows call it from their `alert` job):
#   scripts/live-smoke-alert.sh <workflow name> <run id> <outcome> [--dry-run]
#
# --dry-run reads everything but only prints what it would write.

set -euo pipefail

REPO=${REPO:-unofficialbox/box-gantry}
LABEL=live-smoke-failure

DRY_RUN=false
args=()
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=true ;;
    *) args+=("$arg") ;;
  esac
done
[ "${#args[@]}" -eq 3 ] || { echo "usage: $0 <workflow name> <run id> <outcome> [--dry-run]" >&2; exit 1; }
WORKFLOW=${args[0]} RUN_ID=${args[1]} OUTCOME=${args[2]}

run_url="https://github.com/$REPO/actions/runs/$RUN_ID"
marker="<!-- live-smoke: $WORKFLOW -->"

# Writes go through here, so --dry-run can show them instead.
write() {
  if $DRY_RUN; then
    printf '[dry-run] gh api'; printf ' %q' "$@"; printf '\n'
  else
    gh api "$@" >/dev/null
  fi
}

existing=$(gh api "repos/$REPO/issues?state=open&labels=$LABEL&per_page=100" \
  --jq "[.[] | select((.body // \"\") | contains(\"$marker\"))][0].number // empty")

# The failed jobs and steps of this run, each failed job's log tail after them.
failure_report() {
  local jobs
  jobs=$(gh api "repos/$REPO/actions/runs/$RUN_ID/jobs?per_page=100")
  echo "**Failed:**"
  echo ""
  jq -r '.jobs[] | select(.conclusion == "failure")
    | "- `\(.name)`", (.steps[] | select(.conclusion == "failure") | "  - step: \(.name)")' <<<"$jobs"
  for job in $(jq -r '.jobs[] | select(.conclusion == "failure") | .id' <<<"$jobs"); do
    echo ""
    echo "<details><summary>Log tail of job $job</summary>"
    echo ""
    echo '```text'
    # Drop the timestamps and ANSI colour codes; GitHub masks secrets in logs.
    gh api "repos/$REPO/actions/jobs/$job/logs" 2>/dev/null |
      sed -e 's/^[0-9T:.-]*Z //' -e 's/\x1b\[[0-9;]*m//g' | tail -n 60 || echo "(log not available)"
    echo '```'
    echo ""
    echo "</details>"
  done
}

case "$OUTCOME" in
  failure)
    report=$(failure_report)
    if [ -n "$existing" ]; then
      echo "Live smoke still failing; commenting on #$existing"
      write -X POST "repos/$REPO/issues/$existing/comments" \
        -f body="Still failing in [run $RUN_ID]($run_url).

$report"
    else
      echo "Live smoke failed; opening an issue"
      write -X POST "repos/$REPO/labels" -f name="$LABEL" -f color=D93F0B \
        -f description="A scheduled live smoke against real Box is failing" 2>/dev/null || true
      write -X POST "repos/$REPO/issues" -f title="Live smoke failing: $WORKFLOW" \
        -f "labels[]=$LABEL" -f body="The **$WORKFLOW** workflow failed against real Box in [run $RUN_ID]($run_url).

Likely causes, most common first: an expired or revoked credential in the repo secrets, a change in Box's API behaviour, or a regression on \`main\`. This issue closes itself when the workflow next passes.

$report

$marker"
    fi
    ;;
  success)
    if [ -n "$existing" ]; then
      echo "Live smoke passing again; closing #$existing"
      write -X POST "repos/$REPO/issues/$existing/comments" \
        -f body="Passing again in [run $RUN_ID]($run_url); closing."
      write -X PATCH "repos/$REPO/issues/$existing" -f state=closed -f state_reason=completed
    else
      echo "Live smoke passed; no open issue"
    fi
    ;;
  *)
    echo "Live smoke $OUTCOME; nothing to do"
    ;;
esac
