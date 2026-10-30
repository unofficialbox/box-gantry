#!/usr/bin/env bash
#
# Ship a fleet release end to end: bump SDK_VERSION through a PR, wait for its
# CI, merge it, run the Release workflow as a dry run, then for real. The live
# run's `verify` job confirms the tag on all five SDK repos and the package on
# each registry.
#
# Every step waits for the one before and stops on the first failure. The live
# release is a one-way door — no registry lets you replace a published
# version — so it asks before starting unless --yes is given.
#
# Usage:
#   scripts/ship.sh 0.10.0             # the whole sequence
#   scripts/ship.sh 0.10.0 --yes       # without the prompt before going live
#   scripts/ship.sh 0.10.0 --dry-run   # preflight only; print the plan, change nothing
#
# Requires `gh` authenticated with push access to unofficialbox/box-gantry,
# and a clean checkout.

set -euo pipefail

REPO=unofficialbox/box-gantry
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

# --- Preflight -------------------------------------------------------------
say "Preflight"
gh api "repos/$REPO" --jq .full_name >/dev/null 2>&1 || fail "gh cannot read $REPO; is it authenticated?"
[ -z "$(git status --porcelain)" ] || fail "the working tree has uncommitted changes"
git fetch -q origin main

current=$(git show "origin/main:$MANIFEST" | sed -n 's/^pub const SDK_VERSION: &str = "\(.*\)";$/\1/p')
[ -n "$current" ] || fail "cannot read SDK_VERSION from origin/main:$MANIFEST"
[ "$VERSION" != "$current" ] || fail "main is already at $VERSION"
newest=$(printf '%s\n%s\n' "$current" "$VERSION" | sort -V | tail -1)
[ "$newest" = "$VERSION" ] || fail "$VERSION is older than main's $current"
if git ls-remote --exit-code --tags "https://github.com/unofficialbox/box-open-go-sdk.git" "refs/tags/v$VERSION" >/dev/null 2>&1; then
  fail "v$VERSION is already tagged on box-open-go-sdk"
fi

main_sha=$(git rev-parse origin/main)
read -r ci_sha ci_state <<<"$(gh run list -R "$REPO" -w CI -b main -L 1 \
  --json headSha,conclusion --jq '.[0] | "\(.headSha) \(.conclusion)"')"
[ "$ci_sha" = "$main_sha" ] || fail "CI hasn't run on main's head ${main_sha:0:7} yet"
[ "$ci_state" = "success" ] || fail "CI on main's head ${main_sha:0:7} is '${ci_state:-running}', not green"
echo "main ${main_sha:0:7} is green at $current; shipping $VERSION"

if $DRY_RUN; then
  say "Plan (dry run: nothing changed)"
  echo "1. branch ship-$VERSION from main; SDK_VERSION $current -> $VERSION; PR; wait for CI; squash-merge"
  echo "2. Release workflow, dry run; then, after confirming, the live run"
  echo "3. the live run's verify job checks the five tags and crates.io, npm, Maven Central"
  exit 0
fi

# Wait for the newest run of `$1` on main created after `$2`, then watch it.
watch_new_run() {
  local workflow=$1 since=$2 id=""
  for _ in $(seq 1 30); do
    id=$(gh run list -R "$REPO" -w "$workflow" -b main -L 5 --json databaseId,createdAt \
      --jq "[.[] | select(.createdAt > \"$since\")][0].databaseId // empty")
    [ -n "$id" ] && break
    sleep 5
  done
  [ -n "$id" ] || fail "no new $workflow run appeared"
  echo "https://github.com/$REPO/actions/runs/$id"
  gh run watch "$id" -R "$REPO" --exit-status --interval 30 >/dev/null || fail "$workflow run $id failed"
}

# --- 1. The bump PR --------------------------------------------------------
say "1. Bump SDK_VERSION to $VERSION"
branch="ship-$VERSION"
git switch -q -C "$branch" origin/main
sed -i "s/^pub const SDK_VERSION: &str = \"$current\";$/pub const SDK_VERSION: \&str = \"$VERSION\";/" "$MANIFEST"
git diff --quiet -- "$MANIFEST" && fail "the SDK_VERSION line didn't change"
cargo test -q -p gantry-manifest >/dev/null
git commit -q -am "ship: bump SDK version to $VERSION"
git push -q -u origin "$branch"
pr=$(gh pr create -R "$REPO" --base main --head "$branch" \
  --title "ship: bump SDK version to $VERSION" \
  --body "Bumps \`SDK_VERSION\` from $current to $VERSION for the next fleet release (opened by \`scripts/ship.sh\`).")
echo "$pr"
sleep 10
gh pr checks "$pr" -R "$REPO" --watch --fail-fast >/dev/null || fail "CI failed on $pr"
gh pr merge "$pr" -R "$REPO" --squash --delete-branch >/dev/null
git fetch -q origin main
merged=$(git show "origin/main:$MANIFEST" | sed -n 's/^pub const SDK_VERSION: &str = "\(.*\)";$/\1/p')
[ "$merged" = "$VERSION" ] || fail "main reads SDK_VERSION $merged after the merge, not $VERSION"

# --- 2. Dry run ------------------------------------------------------------
say "2. Release dry run"
since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
gh workflow run release.yml -R "$REPO" --ref main -f dry_run=true
watch_new_run release.yml "$since"

# --- 3. Live ---------------------------------------------------------------
if ! $YES; then
  read -r -p "Publish $VERSION to every registry? This cannot be undone. [y/N] " answer
  [[ "$answer" =~ ^[Yy]$ ]] || fail "stopped before the live release; main is bumped to $VERSION"
fi
say "3. Release $VERSION (live, then verify)"
since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
gh workflow run release.yml -R "$REPO" --ref main -f dry_run=false
watch_new_run release.yml "$since"

say "Shipped $VERSION: tagged on all five repos and verified on every registry"
