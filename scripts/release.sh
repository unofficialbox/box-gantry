#!/usr/bin/env bash
#
# Cut a fleet release: regenerate each SDK into its published repo, then
# commit, tag, push, and cut a GitHub release for all five at once.
#
# The engine already does the hard part. `gantry generate --out <repo>` prunes
# via `.gantry-manifest` — it deletes exactly the files a previous generation
# wrote and no longer emits, and never touches anything it didn't write (`.git`,
# a hand-added `.gitignore`, `package-lock.json`, the Apex `.sf/` config). So
# this script generates straight into the working repo rather than syncing from
# a scratch directory; there is no exclude list to keep correct.
#
# What this script adds is the guard. See `guard_version` below: the 0.1.2
# release shipped new content under an already-published version, which no
# registry will ever let you correct. That is the failure this exists to make
# impossible.
#
# Usage:
#   scripts/release.sh              # cut the release
#   scripts/release.sh --dry-run    # generate + run every guard, change nothing
#
# The fleet lives beside this checkout by default; override with FLEET_ROOT.
# Requires `gh` authenticated against the unofficialbox org.

set -euo pipefail

DRY_RUN=false
[ "${1:-}" = "--dry-run" ] && DRY_RUN=true

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The fleet checkout usually sits beside this one, but how far "beside" depends
# on how deeply box-gantry is nested. Probe the plausible spots rather than
# hard-coding one layout; FLEET_ROOT overrides the search entirely.
if [ -z "${FLEET_ROOT:-}" ]; then
  for candidate in \
    "$(dirname "$REPO_ROOT")/unofficialbox" \
    "$(dirname "$(dirname "$REPO_ROOT")")/unofficialbox" \
    "$(dirname "$REPO_ROOT")"; do
    if [ -d "$candidate/box-open-go-sdk/.git" ]; then
      FLEET_ROOT="$candidate"
      break
    fi
  done
fi
if [ -z "${FLEET_ROOT:-}" ] || [ ! -d "$FLEET_ROOT" ]; then
  echo "error: cannot find the fleet checkout; set FLEET_ROOT to the directory" >&2
  echo "       holding box-open-go-sdk, box-open-rust-sdk, and friends." >&2
  exit 1
fi

SPECS=(
  "$REPO_ROOT/fixtures/specs/openapi.json"
  "$REPO_ROOT/fixtures/specs/openapi-v2025.0.json"
  "$REPO_ROOT/fixtures/specs/openapi-v2026.0.json"
)

# target:repo-directory. The TypeScript repo is `ts`, not `typescript` — the
# one place the manifest key and the repo name diverge.
TARGETS=(
  "go:box-open-go-sdk"
  "rust:box-open-rust-sdk"
  "typescript:box-open-ts-sdk"
  "java:box-open-java-sdk"
  "apex:box-open-apex-sdk"
)

# The single version source (D-187), read rather than passed, so the tag can
# never disagree with what got stamped into the package manifests.
VERSION="$(sed -n 's/^pub const SDK_VERSION: &str = "\(.*\)";$/\1/p' \
  "$REPO_ROOT/crates/gantry-manifest/src/lib.rs")"
if [ -z "$VERSION" ]; then
  echo "error: could not read SDK_VERSION from crates/gantry-manifest/src/lib.rs" >&2
  exit 1
fi
TAG="v$VERSION"

say() { printf '\n\033[1m%s\033[0m\n' "$*"; }
fail() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

repo_path() { echo "$FLEET_ROOT/$1"; }

# Refuse to touch a repo that isn't a clean, current `main`. A release built on
# a dirty tree silently bakes in whatever was lying around.
precheck_repo() {
  local dir="$1" name="$2"
  [ -d "$dir/.git" ] || fail "$name: not a git repo at $dir"
  local branch
  branch="$(git -C "$dir" branch --show-current)"
  [ "$branch" = "main" ] || fail "$name: on '$branch', expected main"
  [ -z "$(git -C "$dir" status --porcelain)" ] \
    || fail "$name: working tree is dirty; commit or stash first"
  git -C "$dir" fetch --quiet --tags origin
  if [ -n "$(git -C "$dir" rev-list "HEAD..origin/main" --count 2>/dev/null)" ] \
    && [ "$(git -C "$dir" rev-list "HEAD..origin/main" --count)" != "0" ]; then
    fail "$name: behind origin/main; pull first"
  fi
}

# The guard this script exists for.
#
# A published version is immutable everywhere that matters — crates.io, npm,
# Maven Central, and a Go module proxy that has already cached the tag. So if
# the regenerated output differs from what the repo holds while $TAG has
# *already* been cut, the release is unshippable: the new content has nowhere
# to go under that number. Fail loudly and name the drift (NF-1) rather than
# pushing content that no consumer of $TAG will ever receive.
guard_version() {
  local dir="$1" name="$2"
  if [ -z "$(git -C "$dir" status --porcelain)" ]; then
    return 0  # nothing changed — a re-run, or a version-only bump (Go).
  fi
  if git -C "$dir" rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    printf '\033[31merror:\033[0m %s: regenerating changed these files, but %s is already tagged:\n' \
      "$name" "$TAG" >&2
    git -C "$dir" status --porcelain | sed 's/^/    /' >&2
    cat >&2 <<EOF

  $TAG has been released; registries will not accept different content under
  it. Bump SDK_VERSION in crates/gantry-manifest/src/lib.rs and re-run.
EOF
    exit 1
  fi
}

# ---------------------------------------------------------------------------
# Phase 1 — prechecks. Every repo is validated before any of them is touched,
# so a bad fifth repo can't leave the first four half-released.
# ---------------------------------------------------------------------------
say "Releasing the fleet at $TAG"
echo "fleet root: $FLEET_ROOT"
$DRY_RUN && echo "mode:       DRY RUN (no commit, tag, push, or release)"

for entry in "${TARGETS[@]}"; do
  name="${entry##*:}"
  precheck_repo "$(repo_path "$name")" "$name"
done
echo "prechecks:  all five repos clean, on main, and current"

# ---------------------------------------------------------------------------
# Phase 2 — generate and guard. Still no pushes: if any repo fails its guard,
# the whole release aborts with nothing published.
# ---------------------------------------------------------------------------
say "Generating"
for entry in "${TARGETS[@]}"; do
  target="${entry%%:*}"; name="${entry##*:}"
  dir="$(repo_path "$name")"
  cargo run --quiet --manifest-path "$REPO_ROOT/Cargo.toml" -p gantry-cli -- \
    generate --target "$target" --out "$dir" "${SPECS[@]}" >/dev/null
  guard_version "$dir" "$name"
  changed="$(git -C "$dir" status --porcelain | wc -l | tr -d ' ')"
  printf '  %-22s %s file(s) changed\n' "$name" "$changed"
done

if $DRY_RUN; then
  say "Dry run complete — every guard passed."
  echo "The working trees now hold the regenerated output; inspect with"
  echo "  git -C $FLEET_ROOT/<repo> diff"
  echo "and 'git checkout .' to discard, or re-run without --dry-run to release."
  exit 0
fi

# ---------------------------------------------------------------------------
# Phase 3 — publish. Guards have passed for all five, so this is the only
# phase that mutates anything remote.
# ---------------------------------------------------------------------------
say "Committing, tagging, and pushing"
NOTES_FILE="$(mktemp)"
trap 'rm -f "$NOTES_FILE"' EXIT
cat >"$NOTES_FILE" <<EOF
## box-open-sdk $VERSION

Generated from the Box OpenAPI specification by
[box-gantry](https://github.com/unofficialbox/box-gantry).

> Not affiliated with, authorized, or endorsed by Box, Inc.
> A community, generated client.
EOF

for entry in "${TARGETS[@]}"; do
  name="${entry##*:}"
  dir="$(repo_path "$name")"

  if [ -n "$(git -C "$dir" status --porcelain)" ]; then
    git -C "$dir" add -A
    git -C "$dir" commit --quiet -m "Release $VERSION

Regenerated from box-gantry at SDK_VERSION $VERSION."
  fi

  # Go carries no in-file version (its version *is* the tag), so a version-only
  # release legitimately produces no commit — tag the existing HEAD.
  if ! git -C "$dir" rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    git -C "$dir" tag -a "$TAG" -m "$TAG"
  fi

  git -C "$dir" push --quiet origin main --tags

  # Re-runnable. A release can fail *after* the fleet is tagged and pushed —
  # 0.2.1 got this far and then died on the crates.io publish — and the retry
  # has to be able to walk back through the git work it already did to reach
  # the registry step that failed. Everything above is naturally idempotent
  # (no changes to commit, tag already present, push a no-op); creating a
  # release that exists is the one call that is not.
  if gh release view "$TAG" --repo "unofficialbox/$name" >/dev/null 2>&1; then
    printf '  %-22s already at %s\n' "$name" "$TAG"
  else
    gh release create "$TAG" --repo "unofficialbox/$name" \
      --title "$TAG" --notes-file "$NOTES_FILE" >/dev/null
    printf '  %-22s pushed + released %s\n' "$name" "$TAG"
  fi
done

say "Fleet released at $TAG"
cat <<EOF
Go is done — its tag is its release. The remaining registries publish from
.github/workflows/release.yml, or by hand:

  crates.io  cd $FLEET_ROOT/box-open-rust-sdk && cargo publish
  npm        cd $FLEET_ROOT/box-open-ts-sdk   && npm install && npm run build && npm publish --access public
  Maven      cd $FLEET_ROOT/box-open-java-sdk && mvn -P release -s ~/.m2/settings.xml clean deploy
EOF
