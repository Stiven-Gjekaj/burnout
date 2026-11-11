#!/usr/bin/env bash
# Make the WIM files that the tests of burnout-wim read.
#
# wimlib-imagex makes them. It is GPL, and it runs here only to make test
# data: no code of it goes into Burnout. Run this script again only to
# change the data, and commit the WIM files with it.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tree="$(mktemp -d)"
trap 'rm -rf "$tree"' EXIT

python3 - "$tree" <<'PY'
import os, sys
root = sys.argv[1]
os.makedirs(f"{root}/docs/deep", exist_ok=True)
# Text that repeats, so LZX finds matches near and far.
words = "burnout writes a bootable drive and reads it back ".split()
text = " ".join(words[(i * 7) % len(words)] for i in range(30000))
open(f"{root}/docs/text.txt", "w").write(text)
# Bytes of x86 CALL instructions, so the E8 translation runs.
calls = bytearray()
for i in range(20000):
    calls += bytes([0xE8]) + (i * 37 % 100000).to_bytes(4, "little") + bytes([0x90, i % 256])
open(f"{root}/calls.bin", "wb").write(calls)
# Bytes that do not compress, so chunks are stored as they are.
state, noise = 12345, bytearray()
for _ in range(70000):
    state = (state * 1103515245 + 12345) % 2**31
    noise.append(state >> 23)
open(f"{root}/noise.bin", "wb").write(noise)
open(f"{root}/docs/deep/small.txt", "w").write("one line\n")
open(f"{root}/empty.txt", "w").close()
PY

wimlib-imagex capture "$tree" "$here/lzx-32k.wim" "Test" --compress=lzx --chunk-size=32768 --no-acls --norpfix >/dev/null
wimlib-imagex capture "$tree" "$here/lzx-64k.wim" "Test" --compress=lzx --chunk-size=65536 --no-acls --norpfix >/dev/null
wimlib-imagex capture "$tree" "$here/xpress-32k.wim" "Test" --compress=xpress --chunk-size=32768 --no-acls --norpfix >/dev/null
wimlib-imagex capture "$tree" "$here/xpress-64k.wim" "Test" --compress=xpress --chunk-size=65536 --no-acls --norpfix >/dev/null

# A small tree of Windows for a drive of Windows To Go: the boot manager
# and its files, a font, a file with two names and a file of the system.
win="$(mktemp -d)"
trap 'rm -rf "$tree" "$win"' EXIT
python3 - "$win" <<'PY'
import os, sys
root = sys.argv[1]
for d in ("Windows/Boot/EFI/en-US", "Windows/Boot/Fonts", "Windows/System32/drivers"):
    os.makedirs(f"{root}/{d}", exist_ok=True)
state, manager = 777, bytearray(b"MZ")
for _ in range(150000):
    state = (state * 1103515245 + 12345) % 2**31
    manager.append(state >> 23)
open(f"{root}/Windows/Boot/EFI/bootmgfw.efi", "wb").write(manager)
open(f"{root}/Windows/Boot/EFI/en-US/bootmgfw.efi.mui", "wb").write(b"MUI" * 3000)
open(f"{root}/Windows/Boot/Fonts/segmono_boot.ttf", "wb").write(b"font" * 5000)
open(f"{root}/Windows/System32/notepad.exe", "wb").write(b"notepad " * 40000)
open(f"{root}/Windows/System32/drivers/small.sys", "wb").write(b"small\n")
os.link(f"{root}/Windows/System32/notepad.exe", f"{root}/Windows/notepad.exe")
PY
wimlib-imagex capture "$win" "$here/to-go.wim" "Windows Test" --compress=lzx --chunk-size=32768 --no-acls --norpfix >/dev/null
