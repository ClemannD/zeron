#!/usr/bin/env bash
# Import the downloaded Developer ID certificate and its existing CSR key.
set -euo pipefail
DIR="$HOME/.zeron-selfhost/apple"
CERT="${1:-$DIR/developer-id.cer}"
KEY="$DIR/developer-id.key"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
[[ -f "$CERT" && -f "$KEY" ]] || { echo "The Developer ID certificate or CSR private key is missing." >&2; exit 2; }
WORK="$(mktemp -d "${TMPDIR:-/tmp}/zeron-signing.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
openssl x509 -inform DER -in "$CERT" -out "$WORK/certificate.pem"
openssl x509 -in "$WORK/certificate.pem" -checkend 86400 -noout
openssl x509 -in "$WORK/certificate.pem" -subject -noout | grep -q 'Developer ID Application:.*7C4DAG233S' || {
  echo "Expected this team's Developer ID Application certificate." >&2; exit 2;
}
openssl pkey -in "$KEY" -pubout -outform DER -out "$WORK/key-public.der"
openssl x509 -in "$WORK/certificate.pem" -pubkey -noout |
  openssl pkey -pubin -outform DER -out "$WORK/cert-public.der"
cmp -s "$WORK/key-public.der" "$WORK/cert-public.der" || {
  echo "The certificate does not match the prepared CSR key." >&2; exit 2;
}
# Import the existing key directly, avoiding PKCS12 format differences between
# OpenSSL and macOS Security. The temporary PKCS1 copy is owner-only and deleted.
umask 077
openssl rsa -in "$KEY" -traditional -out "$WORK/identity.pem" 2>/dev/null
import_item() {
  local result
  if result="$(security import "$@" 2>&1)"; then
    printf '%s\n' "$result"
  elif [[ "$result" == *"already exists"* ]]; then
    echo "Keychain item already present."
  else
    printf '%s\n' "$result" >&2
    return 1
  fi
}
import_item "$WORK/identity.pem" -k "$KEYCHAIN" -f openssl -t priv -T /usr/bin/codesign
import_item "$CERT" -k "$KEYCHAIN" -t cert
security find-identity -v -p codesigning
