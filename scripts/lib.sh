# Shared by bundle.sh and make_dmg.sh (sourced, not run).
#
# Signing identity, in order:
#   1. SIGN_IDENTITY, if set ("-" for ad hoc)
#   2. a valid "Developer ID Application" identity in the keychain (of team
#      PASTED_TEAM_ID, if set): releases are signed this way and notarized
#   3. the self-signed "Pasted Release Signing" certificate (local builds
#      without a Developer ID)
#   4. ad hoc
#
# macOS ties the Accessibility permission to the signature's designated
# requirement. With a Developer ID that is the bundle id plus the team, so
# the permission survives updates; ad-hoc signatures change with every build.

RELEASE_IDENTITY="Pasted Release Signing"

developer_id() {
    security find-identity -v -p codesigning 2>/dev/null |
        sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' |
        while read -r name; do
            if [ -z "${PASTED_TEAM_ID:-}" ] || [ "${name%"($PASTED_TEAM_ID)"}" != "$name" ]; then
                echo "$name"
                break
            fi
        done
}

choose_identity() {
    if [ -n "${SIGN_IDENTITY:-}" ]; then
        return
    fi
    SIGN_IDENTITY=$(developer_id)
    if [ -n "$SIGN_IDENTITY" ]; then
        return
    fi
    if security find-identity -p codesigning | grep -q "\"$RELEASE_IDENTITY\""; then
        SIGN_IDENTITY=$RELEASE_IDENTITY
    else
        echo "warning: no Developer ID or \"$RELEASE_IDENTITY\" identity in the keychain; signing ad hoc" >&2
        echo "warning: users will have to grant Accessibility permission again" >&2
        SIGN_IDENTITY=-
    fi
}

is_developer_id() {
    case "$SIGN_IDENTITY" in "Developer ID Application:"*) return 0 ;; *) return 1 ;; esac
}

# Notarization needs a Developer ID and an App Store Connect API key:
#   PASTED_NOTARY_KEY      path to the .p8 key
#   PASTED_NOTARY_KEY_ID   its key id
#   PASTED_NOTARY_ISSUER   the issuer id
can_notarize() {
    is_developer_id && [ -n "${PASTED_NOTARY_KEY:-}" ] && [ -n "${PASTED_NOTARY_KEY_ID:-}" ] &&
        [ -n "${PASTED_NOTARY_ISSUER:-}" ]
}

# notarize FILE: submits a .zip or .dmg and waits; prints Apple's log and
# fails unless it is accepted.
notarize() {
    out=$(mktemp)
    xcrun notarytool submit "$1" --key "$PASTED_NOTARY_KEY" --key-id "$PASTED_NOTARY_KEY_ID" \
        --issuer "$PASTED_NOTARY_ISSUER" --wait --output-format json > "$out" || true
    status=$(plutil -extract status raw -o - "$out" 2>/dev/null || echo "submission failed")
    id=$(plutil -extract id raw -o - "$out" 2>/dev/null || true)
    rm -f "$out"
    if [ "$status" != "Accepted" ]; then
        echo "error: notarization of $1: $status" >&2
        if [ -n "$id" ]; then
            xcrun notarytool log "$id" --key "$PASTED_NOTARY_KEY" --key-id "$PASTED_NOTARY_KEY_ID" \
                --issuer "$PASTED_NOTARY_ISSUER" >&2 || true
        fi
        return 1
    fi
    echo "Notarized $1"
}
