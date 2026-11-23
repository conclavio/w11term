#!/usr/bin/env bash
# Build the disposable guest on an amd64 Ubuntu 24.04 runner.
set -euo pipefail
export LC_ALL=C DEBIAN_FRONTEND=noninteractive
umask 022
if [[ $# -lt 1 || $# -gt 2 || $EUID != 0 || $(dpkg --print-architecture) != amd64 ]]; then
    echo "Usage: sudo bash tests/folder-sharing/guest/build.sh OUTPUT [resolute|noble] (Ubuntu 24.04 amd64 builder)" >&2
    exit 2
fi
# shellcheck source=/dev/null
source /etc/os-release
[[ $ID == ubuntu && $VERSION_ID == 24.04 ]] || { echo "Ubuntu 24.04 required" >&2; exit 2; }
suite=${2:-resolute}
[[ $suite == resolute || $suite == noble ]] || { echo "Unknown guest suite: $suite" >&2; exit 2; }
here=$(cd -- "$(dirname -- "$0")" && pwd)
mkdir -p -- "$1"
out=$(cd -- "$1" && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
root=$work/root
kernel=$work/kernel
mkdir -p "$kernel" "$work/debs"

apt-get -qq update
apt-get -qq install -y --no-install-recommends mmdebstrap ubuntu-keyring kmod cpio zstd xz-utils

# Use the hosted runner's regional mirrors and fallbacks for the guest too.
# APT can switch mirrors when a metadata or package download stalls.
archive=https://archive.ubuntu.com/ubuntu
security=https://security.ubuntu.com/ubuntu
if [[ -s /etc/apt/apt-mirrors.txt ]]; then
    archive=mirror+file:/etc/apt/apt-mirrors.txt
    security=$archive
fi

# Install SSH and FUSE only in the guest. mmdebstrap runs maintainer scripts
# inside its private root and suppresses service startup during the bootstrap.
# Each guest runs its distribution's packaged SSHFS and setuid FUSE helper.
mmdebstrap --mode=root --variant=minbase --architectures=amd64 \
    --keyring=/usr/share/keyrings/ubuntu-archive-keyring.gpg \
    --aptopt='Acquire::http::Timeout "25"' \
    --aptopt='Acquire::https::Timeout "25"' \
    --aptopt='Acquire::Retries "1"' \
    --include=openssh-server,sshfs,fuse3,busybox-static,iproute2,util-linux,mount \
    --dpkgopt='path-exclude=/usr/share/doc/*' \
    --dpkgopt='path-exclude=/usr/share/man/*' \
    "$suite" "$root" \
    "deb $archive $suite main universe" \
    "deb $archive $suite-updates main universe" \
    "deb $security $suite-security main universe"

# Pick one signed, general-purpose Ubuntu kernel and its own modules. The
# runner's booted kernel may be an Azure flavour, with a different ABI/config.
kernel_pkg=$(apt-cache depends linux-image-virtual |
    awk '$1 ~ /^\|?Depends:$/ && $2 ~ /^linux-image-[0-9.]+-[0-9]+-generic$/ {print $2}')
[[ $kernel_pkg =~ ^linux-image-[0-9.]+-[0-9]+-generic$ ]] ||
    { echo "Could not resolve the GA kernel from linux-image-virtual: $kernel_pkg" >&2; exit 1; }
abi=${kernel_pkg#linux-image-}
extract_package() {
    local pkg=$1
    (cd "$work/debs" && apt-get download "$pkg" && dpkg-deb -x "$pkg"_*.deb "$kernel")
}
extract_package "$kernel_pkg"
extract_package "linux-modules-$abi"
# Some package releases install under /usr/lib; kmod expects /lib/modules.
if [[ ! -e $kernel/lib && -d $kernel/usr/lib/modules ]]; then
    ln -s usr/lib "$kernel/lib"
fi
modules=(virtio_pci virtio_net virtio_rng fuse qemu_fw_cfg)
module_list() {
    local module
    for module in "${modules[@]}"; do
        modprobe -C /dev/null -d "$kernel" -S "$abi" --show-depends "$module" || return 1
    done
}
depmod -b "$kernel" "$abi"
if ! module_list >"$work/module-list"; then
    extract_package "linux-modules-extra-$abi"
    depmod -b "$kernel" "$abi"
    module_list >"$work/module-list"
fi
# Keep only transitive requirements, in insmod order. Uncompress them here
# so the guest's BusyBox needs no host-specific module-compression features.
mkdir -p "$root/boot/modules"
: >"$root/boot/modules/load-order"
while read -r path; do
    name=$(basename "$path")
    name=${name%.zst}; name=${name%.xz}; name=${name%.gz}
    case $path in
        *.ko.zst) zstd -dq -o "$root/boot/modules/$name" "$path" ;;
        *.ko.xz)  xz -dc "$path" >"$root/boot/modules/$name" ;;
        *.ko.gz)  gzip -dc "$path" >"$root/boot/modules/$name" ;;
        *.ko)     cp "$path" "$root/boot/modules/$name" ;;
        *) echo "Unknown module format: $path" >&2; exit 1 ;;
    esac
    echo "$name" >>"$root/boot/modules/load-order"
done < <(awk '$1 == "insmod" && !seen[$2]++ {print $2}' "$work/module-list")

chroot "$root" useradd --create-home --user-group --shell /bin/sh w11fixture
# A locked account is ineligible for public-key login with UsePAM=no.
# This non-crypt value cannot match a password; sshd requires publickey.
chroot "$root" usermod --password 'x' w11fixture
install -m 0755 "$here/init" "$root/init"
install -m 0644 "$here/sshd_config" "$root/etc/ssh/w11term-fixture.conf"
# An initramfs boot has no distro service to populate the mount table link.
ln -sfn /proc/self/mounts "$root/etc/mtab"
rm -f "$root"/etc/ssh/ssh_host_* "$root/etc/machine-id"
rm -rf "$root/var/lib/apt/lists" "$root/var/cache/apt/archives" "$root/var/log"
mkdir -p "$root/var/log" "$root/dev"
[[ -c $root/dev/console ]] || mknod -m 600 "$root/dev/console" c 5 1
[[ -u $root/usr/bin/fusermount3 ]] || { echo 'fusermount3 must be setuid root' >&2; exit 1; }
for tool in findmnt sha256sum sshfs fusermount3 ssh-keygen; do
    [[ -x $root/usr/bin/$tool ]] || { echo "Missing guest tool: $tool" >&2; exit 1; }
done
chroot "$root" sshfs -h >"$work/sshfs-help" 2>&1
grep -F -- '-o passive' "$work/sshfs-help"

install -m 0644 "$kernel/boot/vmlinuz-$abi" "$out/vmlinuz"
(cd "$root"; find . -print0 | sort -z | cpio --null --quiet -o -H newc | gzip -1) >"$out/initramfs.gz"
{
    echo "Kernel: $kernel_pkg"
    echo "Guest suite: $suite"
    (cd "$work/debs"; sha256sum -- *.deb)
    chroot "$root" dpkg-query -W
    (cd "$out"; sha256sum vmlinuz initramfs.gz)
} >"$out/manifest.txt"
ls -lh "$out/vmlinuz" "$out/initramfs.gz"
