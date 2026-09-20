#!/bin/bash
# Builds the P0 test drive: one MBR disk image with a FAT32 partition that
# holds every boot file, and an exFAT partition that holds the install image
# whole.
#
# This is the P0 experiment and NOT the product. It calls diskutil, hdiutil
# and rsync on purpose. Burnout itself may call none of them, because the
# whole design rests on doing this work in its own code on three operating
# systems. See docs/milestones.md.
#
# Usage: scripts/spike-drive.sh <windows.iso> <output.img> [unattend.xml]

set -euo pipefail

ISO="${1:?usage: spike-drive.sh <windows.iso> <output.img> [unattend.xml]}"
IMG="${2:?usage: spike-drive.sh <windows.iso> <output.img> [unattend.xml]}"
UNATTEND="${3:-}"
SIZE_MB=12288
BOOT_MB=1400

cleanup() {
    [ -n "${DEV:-}" ] && hdiutil detach "$DEV" -quiet 2>/dev/null || true
    [ -n "${ISODEV:-}" ] && hdiutil detach "$ISODEV" -quiet 2>/dev/null || true
}
trap cleanup EXIT

echo "==> mounting $ISO"
ATTACH=$(hdiutil attach -readonly -nobrowse "$ISO")
ISODEV=$(echo "$ATTACH" | head -1 | awk '{print $1}')
SRC=$(echo "$ATTACH" | grep -o '/Volumes/.*' | head -1)
echo "    at $SRC"

IMAGE=$(ls "$SRC"/sources/install.* 2>/dev/null | head -1)
[ -n "$IMAGE" ] || { echo "no sources/install.wim or install.esd in the ISO"; exit 1; }
echo "    install image: $(basename "$IMAGE"), $(stat -f%z "$IMAGE") bytes"

echo "==> creating $IMG"
rm -f "$IMG"
dd if=/dev/zero of="$IMG" bs=1m count=0 seek=$SIZE_MB 2>/dev/null
DEV=$(hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount "$IMG" | head -1 | awk '{print $1}')

# Refuse to touch anything that is not this image. A wrong device here costs a
# real disk.
[ "$(diskutil info "$DEV" | awk -F': *' '/Virtual/{print $2}')" = "Yes" ] \
    || { echo "$DEV is not a disk image. Stopping."; exit 1; }
echo "    at $DEV (virtual, confirmed)"

echo "==> partitioning MBR: FAT32 ${BOOT_MB}M + exFAT rest"
diskutil partitionDisk "$DEV" MBR FAT32 BOOT ${BOOT_MB}M ExFAT INSTALL R >/dev/null

echo "==> copying boot files to FAT32"
rsync -a --exclude "sources/$(basename "$IMAGE")" "$SRC/" /Volumes/BOOT/

if [ -n "$UNATTEND" ]; then
    echo "==> adding $(basename "$UNATTEND")"
    cp "$UNATTEND" /Volumes/BOOT/autounattend.xml
fi

echo "==> copying $(basename "$IMAGE") to exFAT"
mkdir -p /Volumes/INSTALL/sources
cp "$IMAGE" /Volumes/INSTALL/sources/

dot_clean -m /Volumes/BOOT /Volumes/INSTALL 2>/dev/null || true
find /Volumes/BOOT /Volumes/INSTALL -name '._*' -delete 2>/dev/null || true

echo "==> result"
df -h /Volumes/BOOT /Volumes/INSTALL | sed 's/^/    /'
ls /Volumes/BOOT/efi/boot/ | sed 's/^/    efi\/boot\//'

diskutil unmountDisk "$DEV" >/dev/null
echo "==> done: $IMG"
