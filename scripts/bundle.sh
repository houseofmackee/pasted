#!/bin/sh
# Builds dist/Pasted.app from a release build and signs it.
#
#   scripts/bundle.sh                 signs with the release certificate if it
#                                     is in the keychain, otherwise ad hoc
#   SIGN_IDENTITY="..." scripts/bundle.sh   any other identity ("-" for ad hoc)
#
# Releases are signed with the self-signed "Pasted Release Signing"
# certificate. macOS ties the Accessibility permission to the signature's
# designated requirement; with this certificate that is the bundle id plus
# the certificate, so the permission survives updates. Ad-hoc signatures
# change with every build, and the permission has to be granted again.
set -eu
cd "$(dirname "$0")/.."

RELEASE_IDENTITY="Pasted Release Signing"
if [ -z "${SIGN_IDENTITY:-}" ]; then
    if security find-identity -p codesigning | grep -q "\"$RELEASE_IDENTITY\""; then
        SIGN_IDENTITY=$RELEASE_IDENTITY
    else
        echo "warning: \"$RELEASE_IDENTITY\" is not in the keychain; signing ad hoc" >&2
        echo "warning: users will have to grant Accessibility permission again" >&2
        SIGN_IDENTITY=-
    fi
fi

cargo build --release
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/Pasted.app

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/pasted "$APP/Contents/MacOS/pasted"
cp macos/Info.plist "$APP/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP/Contents/Info.plist"
cp macos/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

codesign --force --options runtime --timestamp=none --sign "$SIGN_IDENTITY" "$APP"
codesign --verify --strict "$APP"
echo "Built $APP ($VERSION), $(codesign -dr - "$APP" 2>&1 | grep designated)"
