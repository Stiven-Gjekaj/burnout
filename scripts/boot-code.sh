#!/usr/bin/env bash
# Build the boot code of a Windows drive from its source, and check that it
# gives the bytes that Burnout ships.
#
# Burnout includes boot/mbr.bin and boot/fat32.bin of crates/burnout-layout
# in the binary. They come from mbr.asm and fat32.asm beside them, and this
# script proves that, so nobody has to trust a binary in the repository.
# With --write it puts the new bytes in place after a change to the source.
#
# Usage: scripts/boot-code.sh [--write]
#
# It needs nasm.

set -euo pipefail

BOOT="$(cd "$(dirname "$0")/../crates/burnout-layout/boot" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

status=0
for name in mbr fat32; do
    nasm -f bin -o "$WORK/$name.bin" "$BOOT/$name.asm"
    if [[ "${1:-}" == "--write" ]]; then
        cp "$WORK/$name.bin" "$BOOT/$name.bin"
        echo "wrote boot/$name.bin"
    elif cmp -s "$WORK/$name.bin" "$BOOT/$name.bin"; then
        echo "boot/$name.bin is what boot/$name.asm gives"
    else
        echo "boot/$name.bin differs from what boot/$name.asm gives" >&2
        status=1
    fi
done
exit "$status"
