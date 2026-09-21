#!/usr/bin/env bash
# Check an image of the Windows layout with the tools of this host.
#
# The host mounts the image with its own mechanism, checks partition 1 with
# its own check tool, and reads each file back through its own FAT driver.
# Each file has to match the manifest that the layout example wrote, and the
# volume has to hold no file that the manifest does not name.
#
# Usage: scripts/check-layout.sh <image> <manifest> [512|4096]
#
# Linux attaches the image with losetup, at either sector size, and needs
# sudo. macOS attaches it with hdiutil, at 512-byte sectors only.

set -euo pipefail

IMAGE="${1:?usage: check-layout.sh <image> <manifest> [512|4096]}"
MANIFEST="$(cd "$(dirname "${2:?usage: check-layout.sh <image> <manifest> [512|4096]}")" && pwd)/$(basename "$2")"
SECTOR="${3:-512}"
MNT="$(mktemp -d)"
DEV=""
MOUNTED=""

cleanup() {
    if [ -n "$MOUNTED" ]; then
        case "$(uname -s)" in
            Linux) sudo umount "$MNT" ;;
            Darwin) umount "$MNT" ;;
        esac
    fi
    if [ -n "$DEV" ]; then
        case "$(uname -s)" in
            Linux) sudo losetup --detach "$DEV" ;;
            Darwin) hdiutil detach "$DEV" -quiet ;;
        esac
    fi
    rmdir "$MNT"
}
trap cleanup EXIT

case "$(uname -s)" in
    Linux)
        DEV="$(sudo losetup --find --show --partscan --sector-size "$SECTOR" "$IMAGE")"
        echo "==> fsck.fat -n ${DEV}p1"
        sudo fsck.fat -n "${DEV}p1"
        # utf8 gives each long name as UTF-8, which the manifest is in.
        # Without it, the kernel gives a name in its default character set,
        # and a name that is not ASCII is not found.
        sudo mount -o ro,utf8 "${DEV}p1" "$MNT"
        MOUNTED=1
        ;;
    Darwin)
        if [ "$SECTOR" != 512 ]; then
            echo "hdiutil attaches an image of 512-byte sectors only" >&2
            exit 2
        fi
        DEV="$(hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount "$IMAGE" | head -1 | awk '{print $1}')"
        echo "==> fsck_msdos -n ${DEV}s1"
        fsck_msdos -n "${DEV}s1"
        mount -t msdos -o rdonly "${DEV}s1" "$MNT"
        MOUNTED=1
        ;;
    *)
        echo "this script knows Linux and macOS only" >&2
        exit 2
        ;;
esac

echo "==> the hash of each file"
if command -v sha256sum >/dev/null; then
    (cd "$MNT" && sha256sum --check --quiet "$MANIFEST")
else
    (cd "$MNT" && shasum -a 256 --check --quiet "$MANIFEST")
fi

want="$(wc -l < "$MANIFEST" | tr -d ' ')"
got="$(find "$MNT" -type f | wc -l | tr -d ' ')"
if [ "$got" != "$want" ]; then
    echo "the volume holds $got files and the manifest names $want" >&2
    exit 1
fi
echo "==> all $want files match"
