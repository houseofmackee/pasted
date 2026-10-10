#!/bin/sh
# Builds dist/Pasted-<version>-arm64.dmg for direct download (Apple silicon
# only), with a SHA-256 checksum next to it, then mounts it to check it.
#
#   scripts/make_dmg.sh                 builds Pasted.app first (scripts/bundle.sh)
#   SKIP_BUNDLE=1 scripts/make_dmg.sh   packages the existing dist/Pasted.app
#   SIGN_IDENTITY="Developer ID Application: ..." also signs the DMG
#
# The app is not notarized yet, so the DMG includes a note on opening it.
set -eu
cd "$(dirname "$0")/.."

if [ -z "${SKIP_BUNDLE:-}" ]; then
    scripts/bundle.sh
fi
APP=dist/Pasted.app
[ -d "$APP" ] || { echo "error: $APP not found (run scripts/bundle.sh)" >&2; exit 1; }

ARCHS=$(lipo -archs "$APP/Contents/MacOS/pasted")
if [ "$ARCHS" != "arm64" ]; then
    echo "error: expected an arm64-only binary, found: $ARCHS" >&2
    exit 1
fi
codesign --verify --strict "$APP"

VERSION=$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")
DMG="dist/Pasted-$VERSION-arm64.dmg"
WORK=$(mktemp -d)
trap 'hdiutil detach -quiet "$WORK/mnt" 2>/dev/null || true; rm -rf "$WORK"' EXIT

STAGE="$WORK/Pasted"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/Pasted.app"
ln -s /Applications "$STAGE/Applications"
cat > "$STAGE/Read Me First.txt" <<EOF
Pasted $VERSION for Apple silicon Macs (macOS 14 Sonoma or later)

Install: drag Pasted into Applications, then open it from there.

Pasted is open source and not notarized by Apple yet, so the first time
you open it macOS says it cannot check it. To open it anyway:

  1. Open Pasted once and click Done in the warning.
  2. Go to System Settings > Privacy & Security, scroll down and click
     Open Anyway next to the message about Pasted.
  3. Confirm with Open.

If Open Anyway does not appear, run this in Terminal instead and then
open Pasted again:

  xattr -dr com.apple.quarantine /Applications/Pasted.app

You only need to do this once. Pasted lives in the menu bar; press
Shift-Command-V to open your clipboard history.

Source code and checksums: see the release page you downloaded this from.
EOF

rm -f "$DMG" "$DMG.sha256"
hdiutil create -quiet -volname "Pasted $VERSION" -srcfolder "$STAGE" -fs HFS+ -format ULMO -ov "$DMG"
if [ -n "${SIGN_IDENTITY:-}" ]; then
    codesign --force --sign "$SIGN_IDENTITY" "$DMG"
fi

# Check what a user will get: the image is intact and the app inside is
# complete, signed and Apple silicon.
hdiutil verify -quiet "$DMG"
mkdir -p "$WORK/mnt"
# Attaching right after creating can fail with "resource busy"; retry.
for attempt in 1 2 3; do
    hdiutil attach -quiet -nobrowse -readonly -mountpoint "$WORK/mnt" "$DMG" && break
    [ "$attempt" = 3 ] && { echo "error: could not mount $DMG" >&2; exit 1; }
    sleep 2
done
codesign --verify --strict "$WORK/mnt/Pasted.app"
[ "$(lipo -archs "$WORK/mnt/Pasted.app/Contents/MacOS/pasted")" = "arm64" ]
[ -L "$WORK/mnt/Applications" ]
hdiutil detach -quiet "$WORK/mnt"

(cd dist && shasum -a 256 "$(basename "$DMG")" > "$(basename "$DMG").sha256")
echo "Built $DMG ($(du -h "$DMG" | cut -f1 | tr -d ' '))"
cat "$DMG.sha256"
