#!/bin/bash
# Cuts a release: stamps the version, tags it, builds, signs and notarises from
# that tag, uploads the archive to GitHub, and points the Homebrew tap at it.
#
# One command, in the one order that keeps the tag and the download honest:
# the version is written first so `mat --version` says it, the tag is made
# before the build so the binary is built from the commit it claims, the upload
# comes after notarisation so a rejected build never leaves a release with
# nothing in it, and the tap comes last because the formula names a download
# that has to be there.
#
# Usage:
#   make release-publish VERSION=0.2.0
#
# Needs: a Developer ID certificate, the `notarytool` keychain profile (see
# scripts/release.sh), `gh` logged in, and docs/release-notes-<version>.md.
set -euo pipefail

cd "$(dirname "$0")/.."

VERSION="${1:-}"
[ -n "$VERSION" ] || { echo "usage: publish-release.sh <version>   e.g. 0.2.0"; exit 2; }
# `v` belongs on the tag and nowhere else: Cargo's version is a number.
VERSION="${VERSION#v}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "not a version: $VERSION"; exit 2; }
TAG="v$VERSION"
ARCHIVE="dist/mat-$VERSION.tar.gz"
REPO="${MAT_REPO:-philipparndt/mat}"

command -v gh >/dev/null || { echo "gh is not installed — brew install gh"; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "gh is not logged in — gh auth login"; exit 1; }
security find-identity -v -p codesigning | grep -q "Developer ID Application" \
	|| { echo "no Developer ID Application certificate in the keychain"; exit 1; }

# The notes before anything else. Abydos checked them after the tag was pushed,
# and 0.19.1 is what that looks like: a tag on GitHub with no release behind it.
HAND_NOTES="docs/release-notes-$VERSION.md"
if [ ! -f "$HAND_NOTES" ]; then
	echo "no release notes at $HAND_NOTES" >&2
	echo "write them first — grouped by what somebody would notice, a short paragraph each" >&2
	exit 1
fi

# A release is built from what is committed. A dirty tree means the tag would
# point at something nobody else can reproduce.
if ! git diff --quiet || ! git diff --cached --quiet; then
	echo "the working tree has changes — commit or stash them first"
	exit 1
fi
if git rev-parse "$TAG" >/dev/null 2>&1; then
	echo "$TAG already exists"
	exit 1
fi

BRANCH=$(git rev-parse --abbrev-ref HEAD)
echo "==> Releasing $TAG from $BRANCH"

# --- The version mat reports ----------------------------------------------------
CURRENT=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [ "$CURRENT" != "$VERSION" ]; then
	sed -i '' "s/^version = \"$CURRENT\"/version = \"$VERSION\"/" Cargo.toml
	# The lock file carries the workspace's own versions too; `--locked` in the
	# build refuses one that disagrees with Cargo.toml.
	cargo update --workspace --offline --quiet
	git add Cargo.toml Cargo.lock
	git commit -q -m "Release $VERSION"
	echo "    version $CURRENT → $VERSION"
fi

git tag -a "$TAG" -m "mat $VERSION"

# --- Build, sign, notarise --------------------------------------------------------
#
# After the tag, so what is built is what the tag names. If this fails the tag
# is still only local: delete it (git tag -d $TAG), fix, and run again.
scripts/release.sh
test -f "$ARCHIVE" || { echo "expected $ARCHIVE and it is not there"; exit 1; }

# --- Publish ------------------------------------------------------------------------
#
# The tag is pushed before the release is created: `gh release create` on a tag
# GitHub has never seen makes one at whatever the branch happens to be.
git push origin "$BRANCH"
git push origin "$TAG"

NOTES=$(mktemp)
{
	cat "$HAND_NOTES"
	echo
	echo "### Install"
	echo
	echo "    brew tap philipparndt/mat"
	echo "    brew trust philipparndt/mat"
	echo "    brew install mat"
	echo
	echo "Homebrew refuses a formula from a tap outside its own repositories until"
	echo "\`brew trust\` says otherwise; that is what the middle line is."
	echo
	echo "Or download \`$(basename "$ARCHIVE")\`: \`bin/mat\` and \`bin/mat-au\` for Apple silicon"
	echo "and Intel, signed with a Developer ID and notarised, and the sample library in"
	echo "\`share/mat/samples\`, which \`mat\` finds beside itself."
	echo
	echo "    shasum -a 256 -c $(basename "$ARCHIVE").sha256"
} > "$NOTES"

gh release create "$TAG" \
	"$ARCHIVE" "$ARCHIVE.sha256" \
	--repo "$REPO" \
	--title "mat $VERSION" \
	--notes-file "$NOTES"
rm -f "$NOTES"

# --- And the tap ----------------------------------------------------------------------
scripts/update-tap.sh "$VERSION"

echo "==> Published $TAG"
gh release view "$TAG" --repo "$REPO" --json url --jq .url
