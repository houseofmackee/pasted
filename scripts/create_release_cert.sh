#!/bin/sh
# Superseded since 0.3.0: releases are signed with a Developer ID and
# notarized (see scripts/lib.sh and .github/workflows/release.yml). The
# self-signed identity is only a fallback for local builds.
#
# Creates the self-signed release signing identity, imports it into the
# login keychain (so local release builds match CI), and exports it to
# target/release-cert for the release workflow's secrets.
#
# Run once per project, ever: every release must be signed with the same
# certificate, because macOS ties permissions (Accessibility, Screen
# Recording) to it. A new certificate makes every user grant them again.
set -eu
cd "$(dirname "$0")/.."

NAME="Pasted Release Signing"
FILE=Pasted-Release

if security find-identity -p codesigning | grep -q "\"$NAME\""; then
    echo "error: \"$NAME\" is already in the keychain. A new one would change every" >&2
    echo "release's signature. To get its .p12 for the release workflow, export it from" >&2
    echo "Keychain Access (My Certificates > right-click \"$NAME\" > Export) instead." >&2
    exit 1
fi

OUT=target/release-cert
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"

cat > "$WORK/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF

# macOS's LibreSSL writes PKCS#12 files that `security import` accepts
# (OpenSSL 3 defaults to algorithms it rejects). Twenty years: every release
# must be signed by this same certificate.
OPENSSL=/usr/bin/openssl
PASSWORD=$($OPENSSL rand -hex 16)
$OPENSSL req -x509 -newkey rsa:2048 -nodes -days 7300 -config "$WORK/cert.cnf" \
    -keyout "$WORK/key.pem" -out "$WORK/cert.pem" 2>/dev/null
$OPENSSL pkcs12 -export -inkey "$WORK/key.pem" -in "$WORK/cert.pem" \
    -out "$WORK/id.p12" -name "$NAME" -passout "pass:$PASSWORD"
security import "$WORK/id.p12" -k "$HOME/Library/Keychains/login.keychain-db" \
    -P "$PASSWORD" -T /usr/bin/codesign

cp "$WORK/id.p12" "$OUT/$FILE.p12"
base64 -i "$WORK/id.p12" > "$OUT/$FILE.p12.base64"
printf %s "$PASSWORD" > "$OUT/password.txt"

cat <<EOF
Created "$NAME" in the login keychain and exported it to $OUT:

  $FILE.p12         the certificate and private key
  $FILE.p12.base64  for the RELEASE_CERT_P12_BASE64 secret
  password.txt      for the RELEASE_CERT_PASSWORD secret

1. Add both secrets to the GitHub repository:
     gh secret set RELEASE_CERT_P12_BASE64 < $OUT/$FILE.p12.base64
     gh secret set RELEASE_CERT_PASSWORD < $OUT/password.txt
2. Back up $FILE.p12 and its password somewhere safe outside GitHub (a
   password manager). Losing them means a new certificate, which makes every
   user grant permissions again.
3. Delete $OUT afterwards.
EOF
