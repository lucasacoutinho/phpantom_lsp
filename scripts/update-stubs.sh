#!/usr/bin/env bash
#
# Update the pinned phpstorm-stubs version in stubs.lock.
#
# Run this before preparing a new release to pick up the latest stubs:
#
#   ./scripts/update-stubs.sh
#
# The script will:
#   1. Query the GitHub API for the latest commit on master.
#   2. Download the tarball for that commit.
#   3. Compute its SHA-256 hash.
#   4. Write the new stubs.lock file.
#   5. Delete the local stubs/ cache so the next build fetches fresh.

set -euo pipefail

REPO="JetBrains/phpstorm-stubs"
LOCK_FILE="stubs.lock"

cd "$(dirname "$0")/.."

echo "Fetching latest commit SHA for ${REPO} master..."
COMMIT=$(curl -sf \
    -H "Accept: application/vnd.github.v3+json" \
    -H "User-Agent: phpantom-lsp-update" \
    "https://api.github.com/repos/${REPO}/commits/master" \
    | python3 -c "import sys,json; print(json.load(sys.stdin)['sha'])")

if [[ -z "$COMMIT" ]]; then
    echo "Error: failed to fetch commit SHA" >&2
    exit 1
fi

SHORT="${COMMIT:0:10}"
TARBALL_URL="https://github.com/${REPO}/archive/${COMMIT}.tar.gz"

echo "Downloading tarball for ${SHORT}..."
TARBALL=$(mktemp)
trap 'rm -f "$TARBALL"' EXIT

curl -sfL -o "$TARBALL" "$TARBALL_URL"
if [[ ! -s "$TARBALL" ]]; then
    echo "Error: failed to download tarball" >&2
    exit 1
fi

echo "Computing SHA-256..."
HASH=$(sha256sum "$TARBALL" | cut -d' ' -f1)

echo "Writing ${LOCK_FILE}..."
cat > "$LOCK_FILE" <<EOF
# PHPantom stubs lock file — pinned phpstorm-stubs version.
#
# This file is checked into version control and read by build.rs to
# ensure reproducible builds with integrity-verified stubs.
#
# To update, run:  scripts/update-stubs.sh

# The pinned commit SHA on JetBrains/phpstorm-stubs master.
commit = "${COMMIT}"

# SHA-256 hash of the GitHub-generated tarball for the commit above.
sha256 = "${HASH}"
EOF

echo "Removing stubs/ cache so next build fetches fresh..."
rm -rf stubs/

echo ""
echo "Done! Pinned to ${SHORT} (${HASH})"
echo "Run 'cargo build' to fetch and verify the new stubs."
