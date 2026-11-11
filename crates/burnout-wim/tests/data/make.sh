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
