#!/bin/sh
# Stock x86-64 test binary from Herdr's versioned release manifest.
set -eu
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
archive=$tmp/hc.tar.gz
curl --fail --silent --show-error --location --retry 2 --max-time 90 \
    https://releases.herdr.dev/core/0.1.21/herdr-core-linux-x86_64.tar.gz -o "$archive"
printf '%s  %s\n' 78424755c10a40dde38a0bdb55c20ac19706e9ae05ed68fc2acf1fcff0d941c1 "$archive" | sha256sum --check
tar -xzf "$archive" -C "$tmp" herdr-core
cargo build --locked --manifest-path "$here/herdr-probe/Cargo.toml" --target-dir "$tmp/probe"
python3 "$here/herdr_linux.py" "$tmp/herdr-core" "$tmp/probe/debug/w11term-herdr-probe"
