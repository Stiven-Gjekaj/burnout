<div align="center">
  <a href="README.md"><b>Burnout</b></a>
</div>

# Contributing to Burnout

Thanks for your interest in Burnout, a command line tool that writes a bootable
drive on Windows, on macOS, and on Linux.
Contributions of all kinds are welcome: bug reports, documentation fixes, new
image formats, hardware reports, and arguments against a decision that is
already made.

Burnout is in the planning stage.
The toolchain is Rust, and no code exists yet, so this guide names no build
command.
It names the rules that hold when the code arrives.
The [README](README.md) says what the current state is.

## Ways to contribute

- Report a drive, a host, or an image that Burnout handles badly.
- Add support for an image format, a file system, or a boot mode.
- Improve the device list, so that it names a drive the way its owner does.
- Improve the documentation.
- Take on a phase from [docs/roadmap.md](docs/roadmap.md), or argue that the
  order is wrong, with the reason.

Before you start significant work, open an issue to agree on the approach.
This costs you one message and can save you a rewritten pull request.

## The rule that comes before all the others

**A test never writes to a real device.**

This project writes block devices.
A test suite that touches one destroys the disk of the person who runs it, and
it destroys the disk of every reviewer after that.
A continuous integration machine has a system disk, and that disk is a block
device.

Write the test against a file.
Create an image file of the size you need, run the same code path against it,
and read the bytes back.
Every layer above the device handle must accept a file, because that is the
only way to test it.
A function that opens a device by name, deep inside the write path, is a design
fault as well as an untestable one.

A pull request that adds a test which opens a path under `/dev`, a
`\\.\PhysicalDrive` path, or any real disk is refused.
This holds even when the test passes on your machine.

## Three more traps that this project creates

**A test can pass for the wrong reason.**
Write a test that converts a large file into split parts, and confirm first
that the input really is larger than the limit.
A test that starts under the limit and ends under the limit proves nothing, and
nobody can see this by reading it.

**Assert the property, not the byte sequence.**
Check that the partition table parses, that the boot sector holds the signature,
and that the file is at the offset you claim.
Do not compare a whole output image against a stored copy.
A version change in a tool moves a timestamp and the test then fails for a
reason that has nothing to do with your change.

**A size is not a state.**
A write is not complete because the counter reached the size of the source.
The bytes are in a cache until the operating system flushes them.
Report progress from what the code proves, and report completion only after the
flush returns.

## What the code must never do

- Write to a device that the person did not select and confirm.
- Write to the disk that the running system starts from. **No flag allows
  this.**
- Treat a fixed disk as a removable one. `--force` allows a fixed disk as a
  target, and only after the person types the model and the size of the drive
  back. A prompt that takes one keystroke is one that people learn to answer
  without reading.
- Report a verification pass that it did not run.
- Hide the target behind a default. The person names the target every time.

## Coding style

- Match the surrounding code. Small, focused functions and clear names beat
  cleverness.
- Keep the write path free of anything that talks to the person. It takes a
  source, a target handle, and a progress callback, and that is the only reason
  a test can run it.
- Add dependencies sparingly, and say in the pull request why the standard
  library or an existing dependency does not do the job.
- Prefer MIT and Apache 2.0 for a dependency. A component with a copyleft
  licence may still be correct here, but say so in the pull request, because it
  changes what this project may ship in one binary.

## Commit messages and pull requests

[`AGENTS.md`](AGENTS.md) is the full set of rules. These four cause the most
rework.

- **One change per commit, and a feature is many commits.** A commit that says
  "integrate the full feature" is wrong even when the code is right. Split it
  into the steps that a reviewer can read and revert one at a time.
- **Code and its tests go in one commit. Documentation goes in its own.**
- **All text uses Simplified Technical English.** Short sentences, active voice,
  present tense. No em-dashes and no emoji, in source, comments, documentation,
  commit messages, or pull requests.
- **Write the subject in the present tense, with no version number.** A commit
  changes no version.

In the pull request, describe what changed and why, and say how you tested it.
Name the host you tested on.
"It works" is not a test report, because this tool fails differently on each of
the three operating systems it supports.

## Reporting a hardware problem

A drive that does not start is useful information, and it needs detail.
Say which host wrote the drive, which image you used, which firmware mode you
started in, and what the screen showed.
Name the drive and its size.
A report without the firmware mode cannot be acted on, because a drive that
starts in UEFI mode and fails in Legacy BIOS mode is a different bug from one
that fails in both.

## Reporting security issues

Do not open a public issue for a security problem.
See [SECURITY.md](SECURITY.md) for how to report it privately.

## Code of conduct

By taking part in this project you agree to follow the
[Code of Conduct](CODE_OF_CONDUCT.md).
