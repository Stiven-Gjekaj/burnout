#!/usr/bin/env bash
# Write the Scoop manifest of the program with windows, burnout-gui, from the
# SHA256SUMS of one release.
#
# The manifest installs the Windows binary of the release, for x86_64 and for
# arm64, and puts Burnout in the Start menu. It depends on the manifest of
# burnout in the same bucket, because the program starts burnout to write. A
# person puts it into the bucket and commits it there: no bot pushes to a
# bucket.
#
# Usage: scripts/scoop-gui-manifest.sh <version> <SHA256SUMS> [base-url] > burnout-gui.json

set -euo pipefail

USAGE="usage: scoop-gui-manifest.sh <version> <SHA256SUMS> [base-url]"
VERSION="${1:?$USAGE}"
SUMS="${2:?$USAGE}"
BASE="${3:-https://github.com/Stiven-Gjekaj/burnout/releases/download/v${VERSION}}"

digest() {
    local file="burnout-gui-${VERSION}-$1"
    local found
    found="$(awk -v f="$file" '$2 == f { print $1 }' "$SUMS")"
    if [[ ! "$found" =~ ^[0-9a-f]{64}$ ]]; then
        echo "scoop-gui-manifest.sh: $SUMS holds no digest for $file" >&2
        exit 1
    fi
    echo "$found"
}

X64="$(digest x86_64-pc-windows-msvc.exe)"
ARM64="$(digest aarch64-pc-windows-msvc.exe)"

LATER='https://github.com/Stiven-Gjekaj/burnout/releases/download/v$version/burnout-gui-$version'

cat <<EOF
{
    "version": "${VERSION}",
    "description": "Writes a bootable USB drive, in a window",
    "homepage": "https://github.com/Stiven-Gjekaj/burnout",
    "license": "MIT",
    "depends": "stiven-gjekaj/burnout",
    "notes": "Burnout asks Windows for the privilege through UAC when it writes.",
    "architecture": {
        "64bit": {
            "url": "${BASE}/burnout-gui-${VERSION}-x86_64-pc-windows-msvc.exe#/burnout-gui.exe",
            "hash": "${X64}"
        },
        "arm64": {
            "url": "${BASE}/burnout-gui-${VERSION}-aarch64-pc-windows-msvc.exe#/burnout-gui.exe",
            "hash": "${ARM64}"
        }
    },
    "bin": "burnout-gui.exe",
    "shortcuts": [
        [
            "burnout-gui.exe",
            "Burnout"
        ]
    ],
    "checkver": "github",
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "${LATER}-x86_64-pc-windows-msvc.exe#/burnout-gui.exe"
            },
            "arm64": {
                "url": "${LATER}-aarch64-pc-windows-msvc.exe#/burnout-gui.exe"
            }
        }
    }
}
EOF
