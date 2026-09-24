<div align="center">
  <a href="../README.md"><b>Burnout</b></a>
</div>

# Releasing

A release is a decision that a person makes. The workflow does the rest, and
it does nothing until a tag tells it to.

## Before a release

Every item has to be true, and not only probably true.

- CI is green on the commit that the tag will name. That includes the job
  that packages the four crates for crates.io.
- The exit test of the phase that the release belongs to has passed, on the
  hardware it names. [The roadmap](roadmap.md) says which test that is.
- The version in the root `Cargo.toml` is the version of the release.

## Cut a release

1. Set `version` under `[workspace.package]` in the root `Cargo.toml`, and
   the same version for the three crates under `[workspace.dependencies]`.
   Commit it. The commit subject names no version number.
2. Push, and wait for CI to pass on that commit.
3. Tag the commit and push the tag:

       git tag v0.1.0
       git push origin v0.1.0

4. The [release workflow](../.github/workflows/release.yml) builds six
   binaries, hashes them, attests each one, and opens a **draft** release.
   It refuses a tag that disagrees with the version in `Cargo.toml`, because
   that binary would name a release it does not belong to.
5. Open the draft. Read the list of assets: six binaries and `SHA256SUMS`.
   Publish it.
6. Publish the four crates to crates.io, from a clean checkout of the tag, on
   a machine where `cargo login` holds the token:

       git checkout v0.1.0
       cargo publish --workspace --locked

   Cargo publishes them in the order of their dependencies: `burnout-core`,
   `burnout-iso`, `burnout-layout`, and then `burnout`. A version on
   crates.io can be yanked, and never deleted.
7. Write the Homebrew formula from the `SHA256SUMS` of the release, and commit
   it to the tap, [Stiven-Gjekaj/homebrew-tap](https://github.com/Stiven-Gjekaj/homebrew-tap).
   A person writes that commit. No bot pushes to the tap.

       gh release download v0.1.0 --pattern SHA256SUMS --dir release
       scripts/homebrew-formula.sh 0.1.0 release/SHA256SUMS > ../homebrew-tap/Formula/burnout.rb

   Then check it the way a user installs it:

       brew install stiven-gjekaj/tap/burnout
       burnout --version

The draft is the last point where nobody outside the project has the files.
This tool erases drives, so a person looks before anybody can download it.
The crates and the tap come after it for the same reason.

| Asset | Host |
| --- | --- |
| `burnout-<version>-x86_64-unknown-linux-musl` | Linux on x86_64, any distribution |
| `burnout-<version>-aarch64-unknown-linux-musl` | Linux on arm64, any distribution |
| `burnout-<version>-x86_64-apple-darwin` | macOS on Intel |
| `burnout-<version>-aarch64-apple-darwin` | macOS on Apple silicon |
| `burnout-<version>-x86_64-pc-windows-msvc.exe` | Windows on x86_64 |
| `burnout-<version>-aarch64-pc-windows-msvc.exe` | Windows on arm64 |

The Linux binaries carry their own C library, and the Windows binaries carry
the Microsoft C runtime, so none of them needs anything installed beside it.

## Test the workflow without a release

    gh workflow run release.yml

A run started by hand builds all six and hashes them, and it publishes
nothing. It makes no attestation either, because on a public repository an
attestation goes into the public Sigstore log. The files wait in the run as an
artifact named `dist`:

    gh run download <run-id> --name dist

## Check a download

Two checks, and they answer different questions.

The checksum says that the file arrived whole:

    shasum -a 256 -c SHA256SUMS --ignore-missing

The attestation says that this file came out of this repository, from the
commit that the tag names, through the release workflow:

    gh attestation verify burnout-0.1.0-aarch64-apple-darwin --repo Stiven-Gjekaj/burnout

The attestation is the stronger claim, and it is the reason this project has
no code signing certificate. [The roadmap](roadmap.md) records that decision.

### The warning that a browser download brings

A browser marks a downloaded file, and both desktop systems then warn before
they run it. The file is the same file. The warning is about how it arrived.

- **macOS** refuses to start a quarantined binary that no paid certificate
  signed. After both checks above pass, take the mark off:

      xattr -d com.apple.quarantine burnout-0.1.0-aarch64-apple-darwin

- **Windows** SmartScreen shows "Windows protected your PC". Select
  **More info**, then **Run anyway**.

A file fetched with `curl` or `gh release download` carries no mark and brings
no warning.
