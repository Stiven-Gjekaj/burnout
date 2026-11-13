#!/usr/bin/env bash
# Write the winget manifest of the program with windows, burnout-gui, from the
# SHA256SUMS of one release.
#
# The manifest installs the Windows binary of the release as a portable
# command, for x86_64 and for arm64. It depends on the package of burnout of
# the same version, because the program starts burnout to write. A person
# opens the pull request to winget-pkgs: no bot does.
#
# Usage: scripts/winget-gui-manifest.sh <version> <SHA256SUMS> <directory> [base-url]
#
# The three files go into <directory>/manifests/s/Stiven-Gjekaj/BurnoutGUI/
# <version>/, which is where winget-pkgs keeps them.

set -euo pipefail

USAGE="usage: winget-gui-manifest.sh <version> <SHA256SUMS> <directory> [base-url]"
VERSION="${1:?$USAGE}"
SUMS="${2:?$USAGE}"
OUT="${3:?$USAGE}"
BASE="${4:-https://github.com/Stiven-Gjekaj/burnout/releases/download/v${VERSION}}"

ID="Stiven-Gjekaj.BurnoutGUI"
SCHEMA="1.28.0"

# The digest in capitals, as winget-pkgs writes it.
digest() {
    local file="burnout-gui-${VERSION}-$1"
    local found
    found="$(awk -v f="$file" '$2 == f { print $1 }' "$SUMS")"
    if [[ ! "$found" =~ ^[0-9a-f]{64}$ ]]; then
        echo "winget-gui-manifest.sh: $SUMS holds no digest for $file" >&2
        exit 1
    fi
    echo "$found" | tr 'a-f' 'A-F'
}

X64="$(digest x86_64-pc-windows-msvc.exe)"
ARM64="$(digest aarch64-pc-windows-msvc.exe)"

DIR="${OUT}/manifests/s/Stiven-Gjekaj/BurnoutGUI/${VERSION}"
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
- burnout-gui
Dependencies:
  PackageDependencies:
  - PackageIdentifier: Stiven-Gjekaj.Burnout
    MinimumVersion: ${VERSION}
Installers:
- Architecture: x64
  InstallerUrl: ${BASE}/burnout-gui-${VERSION}-x86_64-pc-windows-msvc.exe
  InstallerSha256: ${X64}
- Architecture: arm64
  InstallerUrl: ${BASE}/burnout-gui-${VERSION}-aarch64-pc-windows-msvc.exe
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
PackageName: Burnout GUI
PackageUrl: https://github.com/Stiven-Gjekaj/burnout
License: MIT
LicenseUrl: https://github.com/Stiven-Gjekaj/burnout/blob/main/LICENSE
ShortDescription: Writes a bootable USB drive, in a window
Description: The program with windows of Burnout. It lists the drives, shows what it will erase, and asks for the answer before it writes. It starts burnout through UAC to write, and it checks what it wrote.
Tags:
- bootable
- iso
- usb
ManifestType: defaultLocale
ManifestVersion: ${SCHEMA}
EOF

echo "$DIR"
