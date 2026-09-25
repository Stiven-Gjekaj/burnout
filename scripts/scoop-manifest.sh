#!/usr/bin/env bash
# Write the Scoop manifest of one release, from the SHA256SUMS of that
# release.
#
# The manifest installs the Windows binary of the release, for x86_64 and for
# arm64. It waits for version 1, and the roadmap says why. A person puts it
# into a bucket and commits it there: no bot pushes to a bucket.
#
# Usage: scripts/scoop-manifest.sh <version> <SHA256SUMS> [base-url] > burnout.json
#
# The base URL defaults to the download address of the release on GitHub. A
# test gives another address.

set -euo pipefail

USAGE="usage: scoop-manifest.sh <version> <SHA256SUMS> [base-url]"
VERSION="${1:?$USAGE}"
SUMS="${2:?$USAGE}"
BASE="${3:-https://github.com/Stiven-Gjekaj/burnout/releases/download/v${VERSION}}"

# The digest of one binary of the release, or a stop that names what is
# missing. SHA256SUMS holds lines of "<digest>  <file name>".
digest() {
    local file="burnout-${VERSION}-$1"
    local found
    found="$(awk -v f="$file" '$2 == f { print $1 }' "$SUMS")"
    if [[ ! "$found" =~ ^[0-9a-f]{64}$ ]]; then
        echo "scoop-manifest.sh: $SUMS holds no digest for $file" >&2
        exit 1
    fi
    echo "$found"
}

X64="$(digest x86_64-pc-windows-msvc.exe)"
ARM64="$(digest aarch64-pc-windows-msvc.exe)"

# The address of a later release, for the autoupdate of Scoop. Scoop puts the
# version where $version stands, and it hashes the file itself.
LATER='https://github.com/Stiven-Gjekaj/burnout/releases/download/v$version/burnout-$version'

cat <<EOF
{
    "version": "${VERSION}",
    "description": "Writes a bootable USB drive from an ISO or a disk image",
    "homepage": "https://github.com/Stiven-Gjekaj/burnout",
    "license": "MIT",
    "notes": "Burnout writes a drive only from a shell that runs as Administrator.",
    "architecture": {
        "64bit": {
            "url": "${BASE}/burnout-${VERSION}-x86_64-pc-windows-msvc.exe#/burnout.exe",
            "hash": "${X64}"
        },
        "arm64": {
            "url": "${BASE}/burnout-${VERSION}-aarch64-pc-windows-msvc.exe#/burnout.exe",
            "hash": "${ARM64}"
        }
    },
    "bin": "burnout.exe",
    "checkver": "github",
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "${LATER}-x86_64-pc-windows-msvc.exe#/burnout.exe"
            },
            "arm64": {
                "url": "${LATER}-aarch64-pc-windows-msvc.exe#/burnout.exe"
            }
        }
    }
}
EOF
