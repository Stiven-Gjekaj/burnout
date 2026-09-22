#!/usr/bin/env bash
# Check an image of the Windows layout with the tools of this host.
#
# The host mounts the image with its own mechanism, checks partition 1 with
# its own FAT check tool and partition 2 with its own exFAT check tool, and
# reads each file back through its own drivers. Each file has to match the
# manifest that the layout example wrote for its partition, and each volume
# has to hold no file that its manifest does not name.
#
# Usage: scripts/check-layout.sh <image> <boot-manifest> <install-manifest> [512|4096]
#
# Linux attaches the image with losetup, at either sector size, and needs
# sudo. macOS attaches it with hdiutil, at 512-byte sectors only.

set -euo pipefail

USAGE="usage: check-layout.sh <image> <boot-manifest> <install-manifest> [512|4096]"
IMAGE="${1:?$USAGE}"
absolute() { echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; }
BOOT_MANIFEST="$(absolute "${2:?$USAGE}")"
INSTALL_MANIFEST="$(absolute "${3:?$USAGE}")"
SECTOR="${4:-512}"
BOOT_MNT="$(mktemp -d)"
INSTALL_MNT="$(mktemp -d)"
DEV=""
MOUNTED=()

cleanup() {
    for mnt in ${MOUNTED[@]+"${MOUNTED[@]}"}; do
        case "$(uname -s)" in
            Linux) sudo umount "$mnt" ;;
            Darwin) umount "$mnt" ;;
        esac
    done
    if [ -n "$DEV" ]; then
        case "$(uname -s)" in
            Linux) sudo losetup --detach "$DEV" ;;
            Darwin) hdiutil detach "$DEV" -quiet ;;
        esac
    fi
    rmdir "$BOOT_MNT" "$INSTALL_MNT"
}
trap cleanup EXIT

case "$(uname -s)" in
    Linux)
        DEV="$(sudo losetup --find --show --partscan --sector-size "$SECTOR" "$IMAGE")"
        echo "==> fsck.fat -n ${DEV}p1"
        sudo fsck.fat -n "${DEV}p1"
        echo "==> fsck.exfat -n ${DEV}p2"
        sudo fsck.exfat -n "${DEV}p2"
        # utf8 gives each long name as UTF-8, which the manifest is in.
        # Without it, the FAT driver gives a name in its default character
        # set, and a name that is not ASCII is not found.
        sudo mount -o ro,utf8 "${DEV}p1" "$BOOT_MNT"
        MOUNTED+=("$BOOT_MNT")
        sudo mount -t exfat -o ro,iocharset=utf8 "${DEV}p2" "$INSTALL_MNT"
        MOUNTED+=("$INSTALL_MNT")
        ;;
    Darwin)
        if [ "$SECTOR" != 512 ]; then
            echo "hdiutil attaches an image of 512-byte sectors only" >&2
            exit 2
        fi
        DEV="$(hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount "$IMAGE" | head -1 | awk '{print $1}')"
        echo "==> fsck_msdos -n ${DEV}s1"
        fsck_msdos -n "${DEV}s1"
        echo "==> fsck_exfat -n ${DEV}s2"
        fsck_exfat -n "${DEV}s2"
        mount -t msdos -o rdonly "${DEV}s1" "$BOOT_MNT"
        MOUNTED+=("$BOOT_MNT")
        mount -t exfat -o rdonly "${DEV}s2" "$INSTALL_MNT"
        MOUNTED+=("$INSTALL_MNT")
        ;;
    *)
        echo "this script knows Linux and macOS only" >&2
        exit 2
        ;;
esac

# Compare each file of a mounted volume with its manifest, and count them.
check_files() {
    local mnt="$1" manifest="$2" name="$3"
    echo "==> the hash of each file of $name"
    if command -v sha256sum >/dev/null; then
        (cd "$mnt" && sha256sum --check --quiet "$manifest")
    else
        (cd "$mnt" && shasum -a 256 --check --quiet "$manifest")
    fi
    local want got
    want="$(wc -l < "$manifest" | tr -d ' ')"
    got="$(find "$mnt" -type f | wc -l | tr -d ' ')"
    if [ "$got" != "$want" ]; then
        echo "$name holds $got files and its manifest names $want" >&2
        exit 1
    fi
    echo "==> all $want files of $name match"
}

check_files "$BOOT_MNT" "$BOOT_MANIFEST" "partition 1"
check_files "$INSTALL_MNT" "$INSTALL_MANIFEST" "partition 2"
