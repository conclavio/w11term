#!/bin/sh
# Run on a Linux developer machine or Linux CI runner. An explicit binary avoids the download.
set -eu
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
if [ "$#" -ne 0 ]; then
    exec python3 "$here/zmx_recovery.py" "$@"
fi
case $(uname -sm) in
    'Linux x86_64') arch=x86_64; sha=47319ff9f330b65e14dc799517699d13c5f322820d08abecab99f8b18beaaa2d ;;
    'Linux aarch64') arch=aarch64; sha=868de83abc66dc59a5c864ecdef4d5cb763985eedf8b9c1099af9ce75e51fc6b ;;
    *) echo 'Expected Linux x86_64 or aarch64, or pass --zmx /path/to/zmx' >&2; exit 2 ;;
esac
command -v vim >/dev/null
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
archive=zmx-1.0.1-linux-$arch.tar.gz
curl -fsSL --retry 2 "https://github.com/neurosnap/zmx/releases/download/v1.0.1/$archive" -o "$tmp/$archive"
(cd "$tmp" && printf '%s  %s\n' "$sha" "$archive" | sha256sum -c -)
tar -xzf "$tmp/$archive" -C "$tmp" zmx
python3 "$here/zmx_recovery.py" --zmx "$tmp/zmx"
