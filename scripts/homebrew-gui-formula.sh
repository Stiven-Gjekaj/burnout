#!/usr/bin/env bash
# Write the Homebrew formula of the program with windows, burnout-gui, from
# the SHA256SUMS of one release.
#
# The formula installs the binary of the release, for macOS on each chip and
# for Linux on each architecture. It depends on the formula of burnout from
# the same tap, because the program starts burnout to write. It goes into the
# tap of the project, and a person commits it there: no bot pushes to the tap.
#
# Usage: scripts/homebrew-gui-formula.sh <version> <SHA256SUMS> [base-url] > burnout-gui.rb
#
# The base URL defaults to the download address of the release on GitHub. A
# test gives a file:// address instead.

set -euo pipefail

USAGE="usage: homebrew-gui-formula.sh <version> <SHA256SUMS> [base-url]"
VERSION="${1:?$USAGE}"
SUMS="${2:?$USAGE}"
BASE="${3:-https://github.com/Stiven-Gjekaj/burnout/releases/download/v${VERSION}}"

# The digest of one binary of the release, or a stop that names what is
# missing. SHA256SUMS holds lines of "<digest>  <file name>".
digest() {
    local file="burnout-gui-${VERSION}-$1"
    local found
    found="$(awk -v f="$file" '$2 == f { print $1 }' "$SUMS")"
    if [[ ! "$found" =~ ^[0-9a-f]{64}$ ]]; then
        echo "homebrew-gui-formula.sh: $SUMS holds no digest for $file" >&2
        exit 1
    fi
    echo "$found"
}

MAC_ARM="$(digest aarch64-apple-darwin)"
MAC_INTEL="$(digest x86_64-apple-darwin)"
LINUX_ARM="$(digest aarch64-unknown-linux-gnu)"
LINUX_INTEL="$(digest x86_64-unknown-linux-gnu)"

cat <<EOF
# scripts/homebrew-gui-formula.sh in the Burnout repository writes this file
# from the SHA256SUMS of the release. Change the script, and not this file.
class BurnoutGui < Formula
  desc "Writes a bootable USB drive, in a window"
  homepage "https://github.com/Stiven-Gjekaj/burnout"
  license "MIT"

  # The program starts burnout to write, and refuses a burnout of another
  # version. The two formulas of one tap change together.
  depends_on "stiven-gjekaj/tap/burnout"

  on_macos do
    on_arm do
      url "${BASE}/burnout-gui-${VERSION}-aarch64-apple-darwin"
      sha256 "${MAC_ARM}"
    end
    on_intel do
      url "${BASE}/burnout-gui-${VERSION}-x86_64-apple-darwin"
      sha256 "${MAC_INTEL}"
    end
  end

  on_linux do
    on_arm do
      url "${BASE}/burnout-gui-${VERSION}-aarch64-unknown-linux-gnu"
      sha256 "${LINUX_ARM}"
    end
    on_intel do
      url "${BASE}/burnout-gui-${VERSION}-x86_64-unknown-linux-gnu"
      sha256 "${LINUX_INTEL}"
    end
  end

  def install
    binary = Dir["burnout-gui-*"].first
    chmod 0755, binary
    bin.install binary => "burnout-gui"
  end

  test do
    assert_match "burnout-gui #{version}", shell_output("#{bin}/burnout-gui --version")
  end
end
EOF
