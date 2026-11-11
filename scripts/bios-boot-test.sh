#!/usr/bin/env bash
# Start a drive image that Burnout lays out for Windows on a BIOS, in QEMU
# with SeaBIOS, and check that the boot code loads BOOTMGR and starts it.
#
# The image holds boot/probe.asm of crates/burnout-layout as `bootmgr`,
# behind 300 files with long names, so the boot code walks more than one
# cluster of the root directory. The probe writes one line to the serial
# port and stops QEMU. The test passes when the line says that the probe
# runs at 2000:0000, with the drive in DL and in the BPB, with the first
# sector of partition 1 in the BPB, and with the whole file in memory.
#
# It starts the image as an IDE disk and as a USB stick on xHCI and on EHCI,
# because a USB stick is what a person starts from.
#
# Usage: scripts/bios-boot-test.sh
#
# It needs nasm, qemu-system-x86_64, timeout and cargo.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# layout_image puts partition 1 at 1 MiB, which is sector 2048.
EXPECTED="PROBE cs=2000 dl=80 bpb-dl=80 hidden=00000800 signature=ok end=ok"
# The probe stops QEMU with 0x10 on its isa-debug-exit device, and QEMU
# then ends with (0x10 << 1) | 1.
EXPECTED_EXIT=33

nasm -f bin -o "$WORK/probe.bin" "$ROOT/crates/burnout-layout/boot/probe.asm"

mkdir -p "$WORK/tree/boot" "$WORK/tree/efi"
for i in $(seq -w 1 300); do
    printf 'file %s\n' "$i" > "$WORK/tree/a long file name number $i.txt"
done
cp "$WORK/probe.bin" "$WORK/tree/bootmgr"

(cd "$ROOT" && cargo run -q --release -p burnout-layout --example layout_image -- \
    image "$WORK/tree" "$WORK/drive.img" "$WORK/manifest.txt" > /dev/null)

status=0
start() {
    local name="$1"
    shift
    local serial="$WORK/serial-$name.txt"
    local code=0
    timeout 300 qemu-system-x86_64 -machine pc -m 512 -display none -monitor none \
        -serial "file:$serial" \
        -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
        "$@" > /dev/null 2>&1 || code=$?
    local line
    line="$(tr -d '\r' < "$serial" | grep '^PROBE' || true)"
    if [[ "$code" == "$EXPECTED_EXIT" && "$line" == "$EXPECTED" ]]; then
        echo "$name: $line"
    else
        echo "$name: QEMU ended with $code, and the probe said: ${line:-nothing}" >&2
        status=1
    fi
}

start ide -drive "file=$WORK/drive.img,format=raw,if=ide"
for controller in qemu-xhci usb-ehci; do
    start "usb-$controller" \
        -device "$controller,id=usb" \
        -drive "id=stick,file=$WORK/drive.img,format=raw,if=none" \
        -device usb-storage,bus=usb.0,drive=stick,bootindex=1
done
exit "$status"
