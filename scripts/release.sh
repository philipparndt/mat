#!/bin/bash
# Builds, signs, notarises and packages mat for people who are not on this
# machine: dist/mat-<version>.tar.gz, which the Homebrew formula installs.
#
# The same signing as Abydos: a Developer ID Application certificate, the
# hardened runtime, and the `notarytool` keychain profile. On a machine that
# has no profile, once, interactively:
#
#   xcrun notarytool store-credentials notarytool \
#       --apple-id <Apple ID> --team-id 643R6YSRER --password <app-specific>
#
# What is in the archive:
#
#   mat-<version>/bin/mat             the CLI, universal
#   mat-<version>/bin/mat-au          the Audio Unit host, universal; mat finds it beside itself
#   mat-<version>/share/mat/samples   the sample library `samples:` points at
#
# MatCapture.app is not in it: it is an app bundle with its own identity for the
# audio recording permission, and `make capture` builds it where it is used.
set -euo pipefail

cd "$(dirname "$0")/.."

PROFILE="${NOTARY_PROFILE:-notarytool}"
IDENTITY="${SIGN_IDENTITY:-Developer ID Application}"
ENTITLEMENTS="scripts/entitlements.plist"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$VERSION" ] || { echo "no version in Cargo.toml"; exit 1; }
NAME="mat-$VERSION"
DIST="dist"
STAGE="$DIST/$NAME"
ARCHIVE="$DIST/$NAME.tar.gz"
# A target directory of its own: a release is not a development build that
# happens to be lying around, and it does not wait on an editor's cargo lock.
export CARGO_TARGET_DIR="$DIST/target"

security find-identity -v -p codesigning | grep -q "Developer ID Application" \
	|| { echo "no Developer ID Application certificate in the keychain"; exit 1; }

# --- Build, both processors -------------------------------------------------
#
# The download is the one build whose machine is not known in advance, so it
# runs on Apple silicon and on Intel. Checked with lipo below rather than
# trusted: Abydos 0.20.0 promised an Intel build and shipped none.
echo "==> Building mat $VERSION (arm64 + x86_64)"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
	rustup target list --installed | grep -qx "$target" || rustup target add "$target"
	cargo build --release --locked -p mat-cli --target "$target"
done

echo "==> Building mat-au (arm64 + x86_64)"
swift build -c release --package-path swift --product mat-au --arch arm64 --arch x86_64
MAT_AU="swift/.build/apple/Products/Release/mat-au"

rm -rf "$STAGE" "$ARCHIVE" "$ARCHIVE.sha256"
mkdir -p "$STAGE/bin" "$STAGE/share/mat"
lipo -create -output "$STAGE/bin/mat" \
	"$CARGO_TARGET_DIR/aarch64-apple-darwin/release/mat" \
	"$CARGO_TARGET_DIR/x86_64-apple-darwin/release/mat"
cp "$MAT_AU" "$STAGE/bin/mat-au"
# The library as it is in the checkout, licences included; `-L` so a sample
# that is a link in the checkout is a file in the archive.
cp -RL assets/samples "$STAGE/share/mat/samples"
find "$STAGE/share" -name .DS_Store -delete
cp README.md "$STAGE/"

for binary in "$STAGE/bin/"*; do
	archs=$(lipo -archs "$binary")
	for wanted in arm64 x86_64; do
		case " $archs " in
			*" $wanted "*) ;;
			*) echo "refusing to release: $(basename "$binary") is built for '$archs', which lacks $wanted" >&2; exit 1 ;;
		esac
	done
done

# --- Sign -------------------------------------------------------------------
#
# Every Mach-O, with a timestamp and the hardened runtime, which is what
# notarisation asks for. The entitlement lets both tools load plugins signed
# by other people — see entitlements.plist.
echo "==> Signing with: $IDENTITY"
for binary in "$STAGE/bin/"*; do
	codesign --force --timestamp --options runtime --entitlements "$ENTITLEMENTS" \
		--identifier "de.rnd7.$(basename "$binary")" --sign "$IDENTITY" "$binary"
	codesign --verify --strict --verbose=2 "$binary"
done

# --- Check before Apple does -------------------------------------------------
#
# Notarisation takes minutes to say "Invalid"; every reason it would is
# visible here in a second.
echo "==> Checking every Mach-O before uploading"
FAILED=0
while IFS= read -r -d '' binary; do
	file -b "$binary" | grep -q "Mach-O" || continue
	DETAILS=$(codesign -dvv "$binary" 2>&1)
	if ! grep -q "Authority=Developer ID Application" <<< "$DETAILS"; then
		echo "    NOT signed with a Developer ID: ${binary#"$STAGE/"}"
		FAILED=1
	elif ! grep -q "flags=.*runtime" <<< "$DETAILS"; then
		echo "    no hardened runtime: ${binary#"$STAGE/"}"
		FAILED=1
	fi
done < <(find "$STAGE" -type f -perm -111 -print0)
if [ "$FAILED" -ne 0 ]; then
	echo "Stopping: Apple would reject this, and would take several minutes to say so."
	exit 1
fi
echo "    every executable is signed and hardened"

# --- Does it work? ------------------------------------------------------------
#
# The staged binary, run from the staged tree: it must say the version it is
# released as, and find its sample library in share/ — the installed layout —
# rather than in this checkout, which nobody who installs it has.
echo "==> Trying the staged build"
"$STAGE/bin/mat" --version | grep -qx "mat $VERSION" \
	|| { echo "bin/mat does not say it is $VERSION: $("$STAGE/bin/mat" --version)"; exit 1; }
PROBE=$(mktemp -d)
cat > "$PROBE/probe.song" <<'SONG'
tempo 120
instrument kit samples
  kick "samples:sonic-pi/bd_haus.wav"
pattern beat grid=1/4
  kick x...
track drums
  instrument kit
  play beat
SONG
OUTPUT=$(cd "$PROBE" && env -u MAT_ASSETS "$OLDPWD/$STAGE/bin/mat" render probe.song -o probe.wav 2>&1) || { echo "$OUTPUT"; exit 1; }
rm -rf "$PROBE"
if grep -q "warning" <<< "$OUTPUT"; then
	echo "$OUTPUT"
	echo "Stopping: the staged mat renders with warnings — it does not find its samples in share/mat/samples."
	exit 1
fi
echo "    mat $VERSION runs, and finds its samples"

# --- Notarise -----------------------------------------------------------------
#
# A bare executable cannot be stapled — there is nowhere in it to put a ticket —
# so Gatekeeper asks Apple the first time it sees one. What notarising buys is
# that the answer is yes: a download that ends up quarantined (a browser, a
# cask, AirDrop) runs instead of being refused as from an unidentified
# developer. Homebrew's formulae are not quarantined in the first place.
#
# The status is read, not assumed: `--wait` returns once Apple has answered,
# and "Invalid" is an answer.
echo "==> Notarising (this waits for Apple)"
ZIP="$DIST/$NAME-notarize.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$STAGE/bin" "$ZIP"
RESULT=$(xcrun notarytool submit "$ZIP" --keychain-profile "$PROFILE" --wait --output-format json)
rm -f "$ZIP"
STATUS=$(/usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))' <<< "$RESULT")
ID=$(/usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))' <<< "$RESULT")
if [ "$STATUS" != "Accepted" ]; then
	echo "notarisation says '$STATUS' for $ID; Apple's reasons:" >&2
	xcrun notarytool log "$ID" --keychain-profile "$PROFILE" >&2 || true
	exit 1
fi
echo "    accepted ($ID)"

# And that Apple hands the ticket out, for every binary on both processors —
# which is what a Mac that has never seen it will ask for. Asked of Apple's
# ticket service directly rather than of `spctl`, which answers from a local
# cache: this machine ran bin/mat above, before there was a ticket, and `spctl`
# goes on calling it "Unnotarized Developer ID" long after it is not.
echo "==> Asking Apple for the tickets"
for binary in "$STAGE/bin/"*; do
	for arch in arm64 x86_64; do
		CDHASH=$(codesign -dvvv -a "$arch" "$binary" 2>&1 | sed -n 's/^CDHash=//p')
		FOUND=$(curl -fsS -X POST -H 'Content-Type: application/json' \
			-d "{\"records\":[{\"recordName\":\"2/2/$CDHASH\"}]}" \
			https://api.apple-cloudkit.com/database/1/com.apple.gk.ticket-delivery/production/public/records/lookup \
			| /usr/bin/python3 -c 'import json,sys; print("no" if "serverErrorCode" in json.load(sys.stdin)["records"][0] else "yes")' || echo "unknown")
		case "$FOUND" in
			yes) echo "    $(basename "$binary") $arch: ticket $CDHASH" ;;
			no) echo "Stopping: Apple has no ticket for $(basename "$binary") $arch ($CDHASH)" >&2; exit 1 ;;
			*) echo "    $(basename "$binary") $arch: could not ask Apple (offline?) — not checked" ;;
		esac
	done
done

# --- Package --------------------------------------------------------------------
echo "==> Making $ARCHIVE"
COPYFILE_DISABLE=1 tar -C "$DIST" -czf "$ARCHIVE" "$NAME"
# A checksum beside the archive: the formula pins it, and this is the same file
# that left this machine.
(cd "$DIST" && shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256")

echo "==> Done: $ARCHIVE ($(du -h "$ARCHIVE" | cut -f1))"
cat "$ARCHIVE.sha256"
