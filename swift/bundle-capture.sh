#!/bin/sh
# Builds swift/.build/MatCapture.app, the app bundle around mat-capture.
#
# The bundle gives the audio recording permission its own identity instead of
# the terminal's. Signing with a stable certificate keeps that permission across
# rebuilds; ad-hoc signing (no certificate) asks again after every build.
# Override the identity with MAT_CODESIGN_IDENTITY.
set -eu
cd "$(dirname "$0")"

swift build -c release --product mat-capture

app=.build/MatCapture.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp .build/release/mat-capture "$app/Contents/MacOS/mat-capture"
cp bundle/MatCapture-Info.plist "$app/Contents/Info.plist"

identity=${MAT_CODESIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk -F'"' '/Apple Development/ { print $2; exit }')}
if [ -z "$identity" ]; then
    identity=-
    echo "warning: no Apple Development certificate, signing ad-hoc (permission is asked again after each build)" >&2
fi
codesign --force --sign "$identity" "$app"

echo "built $(pwd)/$app"
echo "run: $(pwd)/$app/Contents/MacOS/mat-capture"
