#!/bin/sh
# Builds dist/Pasted.app from a release build and signs it (see lib.sh for
# which identity is used).
#
#   scripts/bundle.sh
#   SIGN_IDENTITY="..." scripts/bundle.sh   a specific identity ("-" for ad hoc)
set -eu
cd "$(dirname "$0")/.."
. scripts/lib.sh
choose_identity

cargo build --release
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/Pasted.app

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/pasted "$APP/Contents/MacOS/pasted"
cp macos/Info.plist "$APP/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP/Contents/Info.plist"
cp macos/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

# Notarization requires the hardened runtime and a secure timestamp.
if is_developer_id; then
    codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$APP"
else
    codesign --force --options runtime --timestamp=none --sign "$SIGN_IDENTITY" "$APP"
fi
codesign --verify --strict "$APP"
echo "Built $APP ($VERSION), $(codesign -dr - "$APP" 2>&1 | grep designated)"
