#!/usr/bin/env bash
#
# Ship a fleet release end to end: bump SDK_VERSION through a PR, wait for its
# CI, merge it, run the Release workflow as a dry run, then for real. The live
# run's `verify` job confirms the tag on all five SDK repos and the package on
# each registry.
#
# Resumable: every run works out how far the release already got, from GitHub
# rather than local state, and continues from there. Re-run the same command
# after an interruption (a dropped connection, a restarted machine, a failed
# check you've since fixed) and it picks up at the next unfinished step:
#
#   - v<version> tagged on the SDK repos  -> released; re-verify the registries
#   - main already at <version>           -> bumped; dry run, then release
#   - an open ship-<version> PR           -> reuse it; wait for CI, merge
#   - otherwise                           -> start with the bump PR
#
# Every step waits for the one before and stops on the first failure. The live
# release is a one-way door — no registry lets you replace a published
# version — so it asks before starting unless --yes is given.
#
# It talks to GitHub through its REST API (`gh api`, `gh run`, `gh workflow`)
# only, so it also runs where GitHub's GraphQL API isn't reachable.
#
# Usage:
#   scripts/ship.sh 0.10.0             # the whole sequence, or the rest of it
#   scripts/ship.sh 0.10.0 --yes       # without the prompt before going live
#   scripts/ship.sh 0.10.0 --dry-run   # report where the release stands and what
#                                      # would run next; change nothing
#
# Requires `gh` authenticated with push access to unofficialbox/box-gantry,
# and a clean checkout.

set -euo pipefail

REPO=unofficialbox/box-gantry
SDK_REPOS=(go rust ts java apex)
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST=crates/gantry-manifest/src/lib.rs

say() { printf '\n\033[1m%s\033[0m\n' "$*"; }
fail() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

VERSION=""
YES=false
DRY_RUN=false
for arg in "$@"; do
  case "$arg" in
    --yes) YES=true ;;
    --dry-run) DRY_RUN=true ;;
    -*) fail "unknown option $arg" ;;
    *) [ -z "$VERSION" ] || fail "one version, please"; VERSION=$arg ;;
  esac
done
[ -n "$VERSION" ] || fail "usage: scripts/ship.sh <version> [--yes] [--dry-run]"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "$VERSION is not MAJOR.MINOR.PATCH"

cd "$REPO_ROOT"
BRANCH="ship-$VERSION"

main_version() {
  git show "origin/main:$MANIFEST" | sed -n 's/^pub const SDK_VERSION: &str = "\(.*\)";$/\1/p'
}

# How many of the five SDK repos carry the v$VERSION tag.
tagged_count() {
  local n=0 repo
  for repo in "${SDK_REPOS[@]}"; do
    if git ls-remote --exit-code --tags "https://github.com/unofficialbox/box-open-$repo-sdk.git" \
      "refs/tags/v$VERSION" >/dev/null 2>&1; then
      n=$((n + 1))
    fi
  done
  echo "$n"
}

# Wait for the CI run on `$1` (a commit on branch `$2`) to finish; fail unless green.
wait_for_ci() {
  local sha=$1 branch=$2 state="" conclusion=""
  for _ in $(seq 1 180); do
    read -r state conclusion <<<"$(gh run list -R "$REPO" -w CI -b "$branch" -L 10 \
      --json headSha,status,conclusion \
      --jq "[.[] | select(.headSha == \"$sha\")][0] | \"\(.status // \"\") \(.conclusion // \"\")\"")"
    [ "$state" = "completed" ] && break
    sleep 30
  done
  [ "$state" = "completed" ] || fail "CI on ${sha:0:7} didn't finish within 90 minutes"
  [ "$conclusion" = "success" ] || fail "CI on ${sha:0:7} ($branch) is '$conclusion', not green"
}

# Dispatch `$1` on main with the remaining arguments as -f inputs, then watch
# the run it starts.
dispatch_and_watch() {
  local workflow=$1; shift
  local since id="" args=()
  for input in "$@"; do args+=(-f "$input"); done
  since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  gh workflow run "$workflow" -R "$REPO" --ref main "${args[@]}" >/dev/null
  for _ in $(seq 1 30); do
    id=$(gh run list -R "$REPO" -w "$workflow" -b main -e workflow_dispatch -L 5 \
      --json databaseId,createdAt --jq "[.[] | select(.createdAt >= \"$since\")][0].databaseId // empty")
    [ -n "$id" ] && break
    sleep 5
  done
  [ -n "$id" ] || fail "no new $workflow run appeared"
  echo "https://github.com/$REPO/actions/runs/$id"
  gh run watch "$id" -R "$REPO" --exit-status --interval 30 >/dev/null || fail "$workflow run $id failed"
}

# --- Where does the release stand? -----------------------------------------
say "Preflight"
gh api "repos/$REPO" --jq .full_name >/dev/null 2>&1 || fail "gh cannot read $REPO; is it authenticated?"
[ -z "$(git status --porcelain)" ] || fail "the working tree has uncommitted changes"
git fetch -q origin main

current=$(main_version)
[ -n "$current" ] || fail "cannot read SDK_VERSION from origin/main:$MANIFEST"
newest=$(printf '%s\n%s\n' "$current" "$VERSION" | sort -V | tail -1)
[ "$newest" = "$VERSION" ] || fail "$VERSION is older than main's $current"

tagged=$(tagged_count)
if [ "$tagged" -gt 0 ] && [ "$tagged" -lt "${#SDK_REPOS[@]}" ]; then
  fail "v$VERSION is tagged on $tagged of ${#SDK_REPOS[@]} SDK repos; a partial release needs a person, not a re-run"
fi
open_pr=$(gh api "repos/$REPO/pulls?head=unofficialbox:$BRANCH&state=open" --jq '.[0].number // empty')

if [ "$tagged" -eq "${#SDK_REPOS[@]}" ]; then
  stage=verify
  echo "v$VERSION is already tagged on every SDK repo; only the registry check is left"
elif [ "$current" = "$VERSION" ]; then
  stage=release
  echo "main is already at $VERSION; the bump is done"
elif [ -n "$open_pr" ]; then
  stage=pr
  echo "resuming the bump PR #$open_pr"
else
  stage=bump
  echo "main is at $current; shipping $VERSION from the start"
fi

if $DRY_RUN; then
  say "Plan (dry run: nothing changed)"
  case "$stage" in
    bump) echo "1. branch $BRANCH from main; SDK_VERSION $current -> $VERSION; open the PR" ;&
    pr) echo "2. wait for the bump PR's CI; squash-merge it" ;&
    release)
      echo "3. wait for CI on main; Release workflow, dry run"
      echo "4. after confirming: the live release; its verify job checks every tag and registry" ;;
    verify) echo "1. Verify a fleet release for $VERSION: the five tags, crates.io, npm, Maven Central" ;;
  esac
  exit 0
fi

# --- 1. The bump PR --------------------------------------------------------
if [ "$stage" = bump ]; then
  say "1. Bump SDK_VERSION to $VERSION"
  git switch -q -C "$BRANCH" origin/main
  sed -i "s/^pub const SDK_VERSION: &str = \"$current\";$/pub const SDK_VERSION: \&str = \"$VERSION\";/" "$MANIFEST"
  git diff --quiet -- "$MANIFEST" && fail "the SDK_VERSION line didn't change"
  cargo test -q -p gantry-manifest >/dev/null
  git commit -q -am "ship: bump SDK version to $VERSION"
  # A branch left by an interrupted run that never got its PR is replaced:
  # it holds nothing but an earlier copy of this same one-line bump.
  git push -q --force-with-lease -u origin "$BRANCH"
  open_pr=$(gh api -X POST "repos/$REPO/pulls" -f base=main -f head="$BRANCH" \
    -f title="ship: bump SDK version to $VERSION" \
    -f body="Bumps \`SDK_VERSION\` from $current to $VERSION for the next fleet release (opened by \`scripts/ship.sh\`)." \
    --jq .number)
  stage=pr
fi

if [ "$stage" = pr ]; then
  say "2. Merge the bump PR"
  echo "https://github.com/$REPO/pull/$open_pr"
  head_sha=$(gh api "repos/$REPO/pulls/$open_pr" --jq .head.sha)
  wait_for_ci "$head_sha" "$BRANCH"
  gh api -X PUT "repos/$REPO/pulls/$open_pr/merge" -f merge_method=squash -f sha="$head_sha" \
    -f commit_title="ship: bump SDK version to $VERSION (#$open_pr)" >/dev/null
  gh api -X DELETE "repos/$REPO/git/refs/heads/$BRANCH" >/dev/null 2>&1 ||
    echo "note: couldn't delete the $BRANCH branch; delete it on GitHub"
  git fetch -q origin main
  [ "$(main_version)" = "$VERSION" ] || fail "main reads SDK_VERSION $(main_version) after the merge, not $VERSION"
  stage=release
fi

# --- 3. Dry run, then live -------------------------------------------------
if [ "$stage" = release ]; then
  say "3. Release dry run"
  wait_for_ci "$(git rev-parse origin/main)" main
  dispatch_and_watch release.yml dry_run=true

  if ! $YES; then
    read -r -p "Publish $VERSION to every registry? This cannot be undone. [y/N] " answer
    [[ "$answer" =~ ^[Yy]$ ]] || fail "stopped before the live release; re-run to continue from here"
  fi
  say "4. Release $VERSION (live, then verify)"
  dispatch_and_watch release.yml dry_run=false
  say "Shipped $VERSION: tagged on all five repos and verified on every registry"
  exit 0
fi

# --- Already released: re-check the registries ------------------------------
say "Verify $VERSION"
dispatch_and_watch verify-release.yml version="$VERSION"
say "$VERSION is tagged on all five repos and live on every registry"
