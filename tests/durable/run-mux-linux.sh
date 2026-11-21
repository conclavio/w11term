#!/usr/bin/env bash
# Stock clients run on the far side of a temporary loopback OpenSSH server.
set -euo pipefail
here=$(cd -- "$(dirname -- "$0")" && pwd)
out=${1:?usage: run-mux-linux.sh CAPTURE_DIRECTORY}
mkdir -p "$out"
tmp=$(mktemp -d)
cleanup() {
    if [[ -f "$tmp/sshd.pid" ]]; then sudo kill "$(cat "$tmp/sshd.pid")" || true; fi
    rm -rf -- "$tmp"
}
trap cleanup EXIT

fetch() {
    local url=$1 sha=$2 name=$3
    curl -fsSL --retry 3 "$url" -o "$tmp/$name.tgz"
    echo "$sha  $tmp/$name.tgz" | sha256sum --check
    mkdir "$tmp/$name-bin"
    tar -xzf "$tmp/$name.tgz" -C "$tmp/$name-bin"
}
fetch https://github.com/programmersd21/venti/releases/download/v0.2.0/venti-0.2.0-linux-x86_64.tar.gz \
    578a5c424e6146863f8ac33508de1b6934ce20af856e74c50923c8a120b100b1 venti
fetch https://github.com/zellij-org/zellij/releases/download/v0.46.0/zellij-x86_64-unknown-linux-musl.tar.gz \
    a8ee599eb357b6142443c91506a4ece3aa8811cb75dcf2b28d194bd66853c79d zellij

port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
ssh-keygen -q -t ed25519 -N '' -f "$tmp/host"
ssh-keygen -q -t ed25519 -N '' -f "$tmp/client"
cat > "$tmp/sshd.conf" <<EOF
ListenAddress 127.0.0.1
Port $port
HostKey $tmp/host
PidFile $tmp/sshd.pid
AuthorizedKeysFile $tmp/client.pub
AllowUsers $(id -un)
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM yes
StrictModes no
AllowTcpForwarding no
AcceptEnv COLORTERM
EOF
echo "[127.0.0.1]:$port $(cat "$tmp/host.pub")" > "$tmp/known_hosts"
cat > "$tmp/client.conf" <<EOF
Host mux-loopback
    HostName 127.0.0.1
    Port $port
    User $(id -un)
    IdentityFile $tmp/client
    IdentitiesOnly yes
    UserKnownHostsFile $tmp/known_hosts
    StrictHostKeyChecking yes
    BatchMode yes
    SendEnv COLORTERM
EOF
sudo install -d -m 0755 /run/sshd
sudo /usr/sbin/sshd -f "$tmp/sshd.conf" -E "$tmp/sshd.log"
python3 "$here/mux_compat.py" --ssh-config "$tmp/client.conf" \
    --venti "$(find "$tmp/venti-bin" -type f -name venti)" \
    --zellij "$(find "$tmp/zellij-bin" -type f -name zellij)" --out "$out"
