#!/bin/sh
# Builds dist/Pasted.app from a release build and signs it.
#
#   scripts/bundle.sh                 ad-hoc signature (local use)
#   SIGN_IDENTITY="Developer ID Application: ..." scripts/bundle.sh
set -eu
cd "$(dirname "$0")/.."

cargo build --release
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/Pasted.app

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/pasted "$APP/Contents/MacOS/pasted"
cp macos/Info.plist "$APP/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP/Contents/Info.plist"
cp macos/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

codesign --force --options runtime --timestamp=none --sign "${SIGN_IDENTITY:--}" "$APP"
codesign --verify --strict "$APP"
echo "Built $APP ($VERSION)"
