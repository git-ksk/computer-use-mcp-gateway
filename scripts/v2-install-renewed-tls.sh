#!/bin/sh
# Copy an ACME-managed certificate/key into the Hub's fail-closed regular-file
# boundary. The ACME client's symlink layout is intentionally not consumed by
# v2_hub directly.
set -eu

if [ "$#" -ne 4 ]; then
  echo "usage: $0 SOURCE_CERT_PEM SOURCE_KEY_PEM DEST_CERT_PEM DEST_KEY_PEM" >&2
  exit 64
fi
src_cert=$1
src_key=$2
dst_cert=$3
dst_key=$4

test -r "$src_cert"
test -r "$src_key"
cert_dir=$(dirname "$dst_cert")
key_dir=$(dirname "$dst_key")
test "$cert_dir" = "$key_dir" || {
  echo "certificate and key destinations must share a directory" >&2
  exit 64
}
install -d -m 0750 "$cert_dir"
tmp_cert=$(mktemp "$cert_dir/.server.pem.XXXXXX")
tmp_key=$(mktemp "$key_dir/.server.key.XXXXXX")
trap 'rm -f "$tmp_cert" "$tmp_key"' EXIT HUP INT TERM
install -m 0644 "$src_cert" "$tmp_cert"
install -m 0600 "$src_key" "$tmp_key"

# Fail before replacement if OpenSSL cannot parse either object or the public
# keys do not match.
openssl x509 -in "$tmp_cert" -noout >/dev/null
openssl pkey -in "$tmp_key" -noout >/dev/null
cert_pub=$(openssl x509 -in "$tmp_cert" -pubkey -noout | openssl pkey -pubin -outform DER | openssl dgst -sha256)
key_pub=$(openssl pkey -in "$tmp_key" -pubout -outform DER | openssl dgst -sha256)
test "$cert_pub" = "$key_pub" || {
  echo "certificate/private-key mismatch" >&2
  exit 65
}

mv -f "$tmp_cert" "$dst_cert"
mv -f "$tmp_key" "$dst_key"
trap - EXIT HUP INT TERM
