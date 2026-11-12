#!/usr/bin/env bash
# One-time (or after the grant is lost) Box OAuth sign-in for the live smoke.
#
#   BOX_OAUTH_CLIENT_ID=... BOX_OAUTH_CLIENT_SECRET=... scripts/oauth-signin.sh
#
# Prints a consent URL, waits for the redirect on http://localhost:8787/callback
# (register it on the Box OAuth app; BOX_OAUTH_REDIRECT_PORT changes the port),
# then stores the refresh token as the BOX_OAUTH_REFRESH_TOKEN repo secret
# through `gh`. The token is never printed. The client ID and secret may also
# come from a .env file (see .env.sample).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
repo="${GITHUB_REPOSITORY:-unofficialbox/box-gantry}"
dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT
export BOX_OAUTH_REFRESH_TOKEN_FILE="$dir/refresh_token"
export BOX_OAUTH_SIGNIN=1

(cd "$root/runtimes/go" && go test -tags live -run TestOAuthSignIn -v -count=1 -timeout 10m ./gantryruntime/...)

if [ ! -s "$BOX_OAUTH_REFRESH_TOKEN_FILE" ]; then
  echo "No refresh token was written; the secret is unchanged." >&2
  exit 1
fi
gh secret set BOX_OAUTH_REFRESH_TOKEN --repo "$repo" < "$BOX_OAUTH_REFRESH_TOKEN_FILE"
echo "BOX_OAUTH_REFRESH_TOKEN updated on $repo."
