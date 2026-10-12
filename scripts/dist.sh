#!/bin/sh
# Builds everything a GitHub release needs, in dist/:
#
#   Pasted-<version>-arm64.dmg   from scripts/make_dmg.sh (signed, notarized
#                                when the notary key is given, and checked)
#   Pasted-<version>-arm64.zip   the same Pasted.app, zipped with ditto, which
#                                keeps the signature intact
#   SHA256SUMS.txt               checksums of both
#   appcast.xml                  Sparkle's update feed for the zip, signed with
#                                the EdDSA key in the file PASTED_SPARKLE_KEY
#                                (only when that is set)
#
# The release workflow (.github/workflows/release.yml) runs this with the
# Developer ID imported and the notary key set. Run locally, it signs the
# same way as scripts/bundle.sh (see scripts/lib.sh).
set -eu
cd "$(dirname "$0")/.."

scripts/make_dmg.sh
APP=dist/Pasted.app
VERSION=$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")
DMG="Pasted-$VERSION-arm64.dmg"
ZIP="Pasted-$VERSION-arm64.zip"

rm -f "dist/$ZIP" dist/SHA256SUMS.txt dist/appcast.xml
ditto -c -k --sequesterRsrc --keepParent "$APP" "dist/$ZIP"
(cd dist && shasum -a 256 "$DMG" "$ZIP" > SHA256SUMS.txt)
cat dist/SHA256SUMS.txt

if [ -n "${PASTED_SPARKLE_KEY:-}" ]; then
    . scripts/lib.sh
    SIGN_UPDATE="$(sparkle_dir)/bin/sign_update"
    BUILD=$(plutil -extract CFBundleVersion raw "$APP/Contents/Info.plist")
    SIGNATURE=$("$SIGN_UPDATE" --ed-key-file "$PASTED_SPARKLE_KEY" -p "dist/$ZIP")
    LENGTH=$(stat -f %z "dist/$ZIP")
    DATE=$(LC_ALL=C date -u "+%a, %d %b %Y %H:%M:%S +0000")
    # PASTED_UPDATE_BASE_URL serves a test update from elsewhere.
    BASE=${PASTED_UPDATE_BASE_URL:-https://github.com/houseofmackee/pasted/releases/download/v$VERSION}
    # Sparkle compares sparkle:version with CFBundleVersion (the build number).
    cat > dist/appcast.xml <<EOF
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Pasted</title>
    <link>https://github.com/houseofmackee/pasted</link>
    <item>
      <title>Pasted $VERSION</title>
      <pubDate>$DATE</pubDate>
      <sparkle:version>$BUILD</sparkle:version>
      <sparkle:shortVersionString>$VERSION</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>14.0</sparkle:minimumSystemVersion>
      <sparkle:fullReleaseNotesLink>https://github.com/houseofmackee/pasted/releases/tag/v$VERSION</sparkle:fullReleaseNotesLink>
      <description><![CDATA[<p>Pasted $VERSION is available. See the release notes for what changed.</p>]]></description>
      <enclosure url="$BASE/$ZIP" length="$LENGTH" type="application/octet-stream" sparkle:edSignature="$SIGNATURE"/>
    </item>
  </channel>
</rss>
EOF
    # The app requires a signed feed.
    "$SIGN_UPDATE" --ed-key-file "$PASTED_SPARKLE_KEY" dist/appcast.xml
    "$SIGN_UPDATE" --ed-key-file "$PASTED_SPARKLE_KEY" --verify dist/appcast.xml
    echo "Wrote dist/appcast.xml"
fi
