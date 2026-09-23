#!/usr/bin/env bash
# Make ISOs of a tree with the tools of this host, and check that the reader
# of Burnout gives the tree back from each one, byte for byte.
#
# Each ISO also has to read as the file system that the reader has to
# choose for it, so a reader that falls back to a poorer tree fails here.
#
# Usage: scripts/check-iso.sh <tree> <iso_tree>
#
# Linux makes ISOs with xorriso: Rock Ridge with Joliet, Joliet alone, and
# Rock Ridge with a file past 4 GiB, which ISO 9660 keeps in more than one
# extent. macOS makes ISOs with hdiutil: UDF 1.02 and UDF 1.50 beside ISO
# 9660 and Joliet, and ISO 9660 with Joliet alone.

set -euo pipefail

USAGE="usage: check-iso.sh <tree> <iso_tree>"
TREE="${1:?$USAGE}"
READER="${2:?$USAGE}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Read one ISO and compare it with a tree. The reader has to choose the
# file system that the third argument names.
check() {
    local iso="$1" tree="$2" file_system="$3"
    echo "==> $(basename "$iso"): $file_system"
    "$READER" compare "$iso" "$tree" | tee "$WORK/out"
    if ! grep -qF "($file_system)" "$WORK/out"; then
        echo "the reader did not choose $file_system for $(basename "$iso")" >&2
        exit 1
    fi
}

case "$(uname -s)" in
    Linux)
        xorriso -as mkisofs -quiet -R -J -V SAMPLE -o "$WORK/rock-ridge.iso" "$TREE"
        check "$WORK/rock-ridge.iso" "$TREE" "ISO 9660 with Rock Ridge"
        xorriso -as mkisofs -quiet -J -V SAMPLE -o "$WORK/joliet.iso" "$TREE"
        check "$WORK/joliet.iso" "$TREE" "ISO 9660 with Joliet"
        rm "$WORK"/*.iso

        # 4 GiB, 5 MiB and 7 bytes. Each MiB carries its own number, so a
        # reader that joins the extents in the wrong order fails.
        mkdir "$WORK/large"
        python3 - "$WORK/large/install.wim" <<'PY'
import sys
with open(sys.argv[1], "wb") as f:
    for n in range(4096 + 5):
        f.write(n.to_bytes(4, "little") * (1 << 18))
    f.write(b"burnout")
PY
        xorriso -as mkisofs -quiet -iso-level 3 -R -V LARGE -o "$WORK/large.iso" "$WORK/large"
        check "$WORK/large.iso" "$WORK/large" "ISO 9660 with Rock Ridge"
        ;;
    Darwin)
        for version in 1.02 1.50; do
            hdiutil makehybrid -quiet -o "$WORK/udf-$version.iso" "$TREE" \
                -udf -udf-version "$version" -iso -joliet -default-volume-name SAMPLE
            check "$WORK/udf-$version.iso" "$TREE" "UDF"
        done
        hdiutil makehybrid -quiet -o "$WORK/joliet.iso" "$TREE" \
            -iso -joliet -default-volume-name SAMPLE
        check "$WORK/joliet.iso" "$TREE" "ISO 9660 with Joliet"
        ;;
    *)
        echo "this script knows Linux and macOS only" >&2
        exit 2
        ;;
esac
