<div align="center">

### Burnout

_A command line tool that writes a bootable drive, on Windows, on macOS, and on Linux_

<p align="center">
  <img src="https://img.shields.io/badge/status-planning-orange?style=flat-square" alt="Planning stage"/>
  <img src="https://img.shields.io/badge/license-MIT-green?style=flat-square" alt="MIT License"/>
</p>

<p align="center">
  <a href="#the-state-of-the-project"><b>State</b></a> |
  <a href="#the-goal"><b>Goal</b></a> |
  <a href="#planned-features"><b>Features</b></a> |
  <a href="#contributing"><b>Contributing</b></a>
</p>

</div>

---

## The state of the project

**Nothing is built yet.**
This repository holds the rules and the licence, and no code.
The toolchain is not chosen.

Do not install this.
There is no release.

---

## The goal

Burnout writes a disk image to a drive from the command line.
It runs on Windows, on macOS, and on Linux, and it behaves the same way on all
three.

A copy of the files out of an ISO file is often not enough for Windows.
The install image can be too large for FAT32.
The Windows 11 installer can refuse the computer.
Old firmware needs boot code that the ISO file does not carry.
Rufus does this work on Windows, and it does it well.
Burnout aims for the same result from a terminal, on any of the three hosts.

---

## Planned features

None of these exists today.
Each one is a target, and the list is the plan rather than a promise.

- **Write a bootable Windows installer** from an ISO file.
- **Split a large install image**, so that a FAT32 drive holds it.
- **Start in UEFI mode and in Legacy BIOS mode** from one drive.
- **Skip the Windows 11 hardware checks**, when the person asks for it.
- **Skip the online account step** in Windows Setup, when the person asks.
- **Make a Windows To Go drive**, which runs Windows from the drive itself.
- **Verify the write**, by reading the drive back and comparing it.
- **Write a Linux or BSD image** as a plain copy.
- **List the drives** with a name that the owner recognises.
- **Refuse a system disk**, every time, unless the person forces the choice.

---

## Warning

Burnout erases the drive that you give it.
The erased data does not go anywhere, and no undo exists.
Read [TERMS.md](TERMS.md) section 4 before you run it.

You supply the operating system image.
Burnout downloads no operating system, and it gives you no licence for one.

---

## Contributing

- [CONTRIBUTING.md](CONTRIBUTING.md) says how to take part, and holds the rule
  that no test writes to a real device.
- [AGENTS.md](AGENTS.md) sets the rules for anybody who changes this
  repository, human or agent.
- [SECURITY.md](SECURITY.md) holds the threat model and how to report a
  vulnerability privately.
- [SUPPORT.md](SUPPORT.md) says where to ask a question.
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) applies to everybody who takes part.

---

## Licence

MIT. See [LICENSE](LICENSE) and [TERMS.md](TERMS.md).
