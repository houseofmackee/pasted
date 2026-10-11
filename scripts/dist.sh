#!/bin/sh
# Builds everything a GitHub release needs, in dist/:
#
#   Pasted-<version>-arm64.dmg   from scripts/make_dmg.sh (signed and checked)
#   Pasted-<version>-arm64.zip   the same Pasted.app, zipped with ditto, which
#                                keeps the signature intact
#   SHA256SUMS.txt               checksums of both
#
# The release workflow (.github/workflows/release.yml) runs this with
# SIGN_IDENTITY="Pasted Release Signing". Run locally, it signs the same way
# as scripts/bundle.sh (the release certificate if present, else ad hoc).
set -eu
cd "$(dirname "$0")/.."

scripts/make_dmg.sh
APP=dist/Pasted.app
VERSION=$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")
DMG="Pasted-$VERSION-arm64.dmg"
ZIP="Pasted-$VERSION-arm64.zip"

rm -f "dist/$ZIP" dist/SHA256SUMS.txt
ditto -c -k --sequesterRsrc --keepParent "$APP" "dist/$ZIP"
(cd dist && shasum -a 256 "$DMG" "$ZIP" > SHA256SUMS.txt)
cat dist/SHA256SUMS.txt
