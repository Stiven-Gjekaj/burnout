#!/usr/bin/env bash
# Write the winget manifest of one release, from the SHA256SUMS of that
# release.
#
# The manifest installs the Windows binary of the release as a portable
# command, for x86_64 and for arm64. It waits for version 1, and the roadmap
# says why. A person opens the pull request to winget-pkgs: no bot does.
#
# Usage: scripts/winget-manifest.sh <version> <SHA256SUMS> <directory> [base-url]
#
# The three files go into <directory>/manifests/s/Stiven-Gjekaj/Burnout/
# <version>/, which is where winget-pkgs keeps them. The base URL defaults to
# the download address of the release on GitHub.

set -euo pipefail

USAGE="usage: winget-manifest.sh <version> <SHA256SUMS> <directory> [base-url]"
VERSION="${1:?$USAGE}"
SUMS="${2:?$USAGE}"
OUT="${3:?$USAGE}"
BASE="${4:-https://github.com/Stiven-Gjekaj/burnout/releases/download/v${VERSION}}"

ID="Stiven-Gjekaj.Burnout"
SCHEMA="1.12.0"

# The digest of one binary of the release in capitals, as winget-pkgs writes
# it, or a stop that names what is missing. SHA256SUMS holds lines of
# "<digest>  <file name>".
digest() {
    local file="burnout-${VERSION}-$1"
    local found
    found="$(awk -v f="$file" '$2 == f { print $1 }' "$SUMS")"
    if [[ ! "$found" =~ ^[0-9a-f]{64}$ ]]; then
        echo "winget-manifest.sh: $SUMS holds no digest for $file" >&2
        exit 1
    fi
    echo "$found" | tr 'a-f' 'A-F'
}

X64="$(digest x86_64-pc-windows-msvc.exe)"
ARM64="$(digest aarch64-pc-windows-msvc.exe)"

DIR="${OUT}/manifests/s/Stiven-Gjekaj/Burnout/${VERSION}"
mkdir -p "$DIR"

cat > "${DIR}/${ID}.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.version.${SCHEMA}.schema.json
PackageIdentifier: ${ID}
PackageVersion: ${VERSION}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: ${SCHEMA}
EOF

cat > "${DIR}/${ID}.installer.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.installer.${SCHEMA}.schema.json
PackageIdentifier: ${ID}
PackageVersion: ${VERSION}
InstallerType: portable
Commands:
- burnout
Installers:
- Architecture: x64
  InstallerUrl: ${BASE}/burnout-${VERSION}-x86_64-pc-windows-msvc.exe
  InstallerSha256: ${X64}
- Architecture: arm64
  InstallerUrl: ${BASE}/burnout-${VERSION}-aarch64-pc-windows-msvc.exe
  InstallerSha256: ${ARM64}
ManifestType: installer
ManifestVersion: ${SCHEMA}
EOF

cat > "${DIR}/${ID}.locale.en-US.yaml" <<EOF
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.defaultLocale.${SCHEMA}.schema.json
PackageIdentifier: ${ID}
PackageVersion: ${VERSION}
PackageLocale: en-US
Publisher: Stiven Gjekaj
PublisherUrl: https://github.com/Stiven-Gjekaj
PackageName: Burnout
PackageUrl: https://github.com/Stiven-Gjekaj/burnout
License: MIT
LicenseUrl: https://github.com/Stiven-Gjekaj/burnout/blob/main/LICENSE
ShortDescription: Writes a bootable USB drive from an ISO or a disk image
Description: Burnout writes a Linux ISO byte for byte, and lays out a Windows ISO on FAT32 and exFAT. It checks what it wrote, and it refuses the disk that the system starts from. It writes a drive only from a shell that runs as Administrator.
Tags:
- bootable
- iso
- usb
ManifestType: defaultLocale
ManifestVersion: ${SCHEMA}
EOF

echo "$DIR"
