<div align="center">

![Burnout](assets/wordmark.svg)

### Writes a bootable USB drive from the command line

_Two commands. The same two on Windows, on macOS, and on Linux._

![Rust](https://img.shields.io/badge/rust-dc2626?style=for-the-badge&logo=rust&logoColor=0c0706)
![Windows](https://img.shields.io/badge/windows-f97316?style=for-the-badge&logo=windows&logoColor=0c0706)
![macOS](https://img.shields.io/badge/macos-fbbf24?style=for-the-badge&logo=apple&logoColor=0c0706)
![Linux](https://img.shields.io/badge/linux-facc15?style=for-the-badge&logo=linux&logoColor=0c0706)
[![MIT licence](https://img.shields.io/badge/mit_licence-fde68a?style=for-the-badge&logoColor=0c0706)](LICENSE)

![Phase](https://img.shields.io/badge/phase-P7_done-f97316?style=flat-square&labelColor=0c0706)
[![Release](https://img.shields.io/github/v/release/Stiven-Gjekaj/burnout?style=flat-square&labelColor=0c0706&color=78350f&label=release)](https://github.com/Stiven-Gjekaj/burnout/releases/latest)

<p align="center">
  <a href="#overview"><b>Overview</b></a> |
  <a href="#the-two-modes"><b>Modes</b></a> |
  <a href="#how-burnout-chooses"><b>Detection</b></a> |
  <a href="#the-layout-that-windows-mode-writes"><b>Layout</b></a> |
  <a href="#the-shape-of-the-command"><b>Commands</b></a> |
  <a href="docs/milestones.md"><b>Milestones</b></a>
</p>

</div>

---

> [!NOTE]
> **Both modes are built.**
> `burnout list` and `burnout write` run on Windows, on macOS and on Linux.
> `write` reads the image first. It copies a hybrid image to a drive, flushes
> it, reads it back and compares a hash. It lays a Windows ISO out for Windows,
> and reads each file back against its own hash.
> Version 1.0.0 is out, and [Install](#install) says how to get it.
> [docs/roadmap.md](docs/roadmap.md) holds the measurements behind both
> claims.
> [docs/roadmap.md](docs/roadmap.md) says what comes next, and
> [docs/milestones.md](docs/milestones.md) holds every decision and the reason
> behind it.

---

## Overview

**Burnout** writes a disk image to a drive.
You give it an image and a drive, and it makes that drive start.

Most tools of this kind solve one half of the problem.
A byte copy writes a Linux ISO and fails on Windows.
Rufus builds a real Windows installer and runs on Windows only.
So the answer today depends on which image you hold and which computer you sit
at, and each answer has its own flags to learn.

Burnout removes the choice.
It reads the image, works out what that image needs, and does it.
The commands you type on a Mac are the commands you type on Windows, and the
image decides the rest.

<table>
<tr>
<td width="50%" valign="top">

### What it aims to do

- **Write any bootable image.** A Linux ISO, a BSD ISO, a raw `.img`.
- **Build a real Windows installer**, and not a copy of the files.
- **Hold a large install image**, with no split and no lost `install.esd`.
- **Skip the Windows 11 checks** for the TPM, Secure Boot and the RAM.
- **Skip the Microsoft account step** in Windows Setup.
- **Verify the write**, and name the check that it ran.

</td>
<td width="50%" valign="top">

### How it aims to behave

- The same commands on all three operating systems.
- No flag for a thing that the tool can work out by itself.
- It never mounts a drive, and it calls no tool of the host.
- It refuses the disk that your system starts from.
- Each error says what went wrong, and then what to do about it.
- It asks for a password through `sudo`, and never reads one itself.
- It names the target, and you confirm it, one time.

</td>
</tr>
</table>

---

## Install

On macOS or on Linux, with Homebrew:

```bash
brew install stiven-gjekaj/tap/burnout
```

On Windows, with Scoop:

```powershell
scoop bucket add stiven-gjekaj https://github.com/Stiven-Gjekaj/scoop-bucket
scoop install stiven-gjekaj/burnout
```

On Windows, with winget, when Microsoft accepts the manifest in
[winget-pkgs](https://github.com/microsoft/winget-pkgs/pull/441640):

```powershell
winget install Stiven-Gjekaj.Burnout
```

With Cargo, on any of the three hosts:

```bash
cargo install burnout
```

Or download the binary for your host from the
[latest release](https://github.com/Stiven-Gjekaj/burnout/releases/latest).
[docs/releasing.md](docs/releasing.md) says how to check it against
`SHA256SUMS` and against its build provenance.

### The warning of a download

A browser marks each file that it downloads, and macOS and Windows then warn
before they start it. The warning is about how the file arrived, and not about
what the file holds. Check the file first, and then take the mark off.

- **macOS** does not start the binary, because no paid certificate signed it.
  Take the mark off, and it starts:

  ```bash
  xattr -d com.apple.quarantine burnout-1.0.0-aarch64-apple-darwin
  ```

- **Windows** SmartScreen can show "Windows protected your PC". Select
  **More info**, and then **Run anyway**. In PowerShell, `Unblock-File` takes
  the mark off:

  ```powershell
  Unblock-File .\burnout-1.0.0-x86_64-pc-windows-msvc.exe
  ```

Homebrew, Cargo, `curl` and `gh release download` set no mark, so they bring
no warning.

---

## The two modes

A drive gets one of two treatments.
They share the device handling and the safety checks, and they share nothing
else.

| | **Raw mode** | **Windows mode** |
| :-- | :-- | :-- |
| **For** | Linux, BSD, any hybrid ISO, any `.img` | A Windows installer ISO |
| **What it does** | Copies the image byte for byte | Partitions, formats, writes files |
| **Partition table** | Comes from inside the image | Burnout creates it |
| **Verifies by** | One hash of the device against the source | One hash for each file |
| **Proves** | The drive holds the image | Each file arrived whole |

A Linux ISO is already a bootable disk image.
It carries its own boot code, its own partition table and its own EFI system
partition inside the file, so writing it means a copy of the bytes.

A Windows ISO is not a bootable disk image.
It holds no boot code for a USB drive, so a byte copy gives a drive that most
firmware refuses.
That one difference is why Windows mode exists, and why a tool like Rufus
exists at all.

---

## How Burnout chooses

You do not tell it. It reads the first 512 bytes.

```mermaid
flowchart LR
    A[the image] --> B{hybrid boot<br/>table?}
    B -- yes --> C([Raw mode])
    B -- no --> D{install.wim<br/>inside?}
    D -- yes --> E([Windows mode])
    D -- no --> F([stop])
```

A boot signature and a partition table in those 512 bytes mean a hybrid image,
so Burnout copies the bytes.
Without them, it looks inside for `sources/install.wim` or
`sources/install.esd` and builds the installer.
If it finds neither, it stops and says what it found.

`--mode raw` and `--mode windows` override the result.
They exist for the day the guess is wrong, and a normal person never types
them.

> [!NOTE]
> **Burnout runs on macOS. It does not make macOS install media.**
> Apple ships no ISO, and the supported path is `createinstallmedia`, which
> exists on macOS only. Burnout detects an `.app` bundle, an
> `InstallAssistant.pkg` or a compressed dmg and refuses with a sentence that
> names the right tool, rather than writing a drive that starts nothing. This
> is a non-goal and not a thing to come later.
> [The milestones](docs/milestones.md) say why.

---

## The layout that Windows mode writes

```
        +=================================================================+
        |  MBR partition table                                            |
        +===============================+=================================+
        |  Partition 1                  |  Partition 2                    |
        |  FAT32, about 1 GB            |  exFAT, the rest of the drive   |
        |                               |                                 |
        |  every boot file              |  sources/install.wim            |
        |  bootmgr, efi/, boot.wim      |  or sources/install.esd         |
        |  autounattend.xml             |  whole, and never split         |
        +===============================+=================================+
                    ^                                  ^
                    |                                  |
          UEFI firmware reads this.           Windows PE reads this,
          It reads FAT only, so every         after it has started from
          boot file lives here.               partition 1.
```

Three decisions hold this together, and [the milestones](docs/milestones.md)
record the reason for each.

- **exFAT, and not NTFS.** macOS mounts NTFS read only, so an NTFS partition
  would give a Windows path that works on two hosts out of three.
- **No file is ever split.** exFAT has no 4 GiB limit, so the install image
  goes on whole. Burnout writes no WIM file, which is also why it stays MIT and
  needs no wimlib.
- **MBR, and not GPT.** Burnout writes the table itself, so the spare EFI
  partition that `diskutil` adds on macOS never appears. It also leaves the
  door open for Legacy BIOS boot later.

---

## The shape of the command

A device path cannot be the same on three operating systems.
`/dev/disk4`, `/dev/sdb` and `\\.\PhysicalDrive2` have nothing in common.
So a device path is never the target.

```bash
burnout list
```

```
#  DRIVE                   SIZE                              BUS     REMOVABLE
1  APPLE SSD AP0512Z       500.3 GB (500,277,792,768 bytes)  Fabric  no   (system disk)
2  Samsung PSSD T7 Shield  1.0 TB (1,000,204,886,016 bytes)  USB     yes
```

The size comes twice. The rounded figure is what a person recognises from the
box, and the exact count is the number that the code can prove.
A drive is sold in powers of ten, so the rounded figure uses them too.
A drive that takes no write, such as an SD card with its lock switch on,
carries the mark `(read only)`, and `write` refuses it.

```bash
burnout write ubuntu-24.04.iso 2
```

That command is identical on all three hosts.

`list` runs with no privilege, so you see your drives before you give a
password. `write` asks for one only when the drive itself refuses the write,
and it asks through `sudo` and never reads a password itself. On Windows it
says to open a shell as Administrator, because a second console window that
closes at the end is worse than a clean refusal.

It names the drive, the size to the byte and the serial, and waits.

```
This erases the drive. Nothing undoes it.

    image   ubuntu-24.04.iso
            2.1 GB (2,109,796,352 bytes)
    drive   SanDisk Ultra
            62.5 GB (62,521,344,000 bytes)
            /dev/rdisk4 USB removable
            serial 4C530001260305117454

The image carries a boot table in its first sector.

Type yes to go on:
>
```

A drive that Burnout cannot prove is removable needs `--force`, and then the
prompt asks for the model and the size of the drive rather than one word.
Nothing allows a write to the drive that the system starts from.

An ISO records the size of its own volume, and Burnout refuses a file that is
shorter before it reads anything else. A download that stopped early leaves
such a file, and a byte copy of it would pass its own check.

```
burnout: Win11.iso holds 3,000,000,000 bytes, and its file system says that it holds 7,994,415,104. The image is incomplete. Download it again, and compare its SHA-256 with the one that its publisher gives
```

When it finishes it names the check that it ran:

```
Wrote 2.1 GB (2,109,796,352 bytes) to SanDisk Ultra.
Checked 2.1 GB (2,109,796,352 bytes) of the drive against the image, byte for byte.
SHA-256 162ba3c552a2d241c7c63ec26777af0255ee1b5a135adc0be986ceed999933ef
macOS ejected the drive, so nothing mounts it or writes to it until you connect it again.
```

The last line comes on macOS only. macOS mounts a drive as soon as nothing
holds it, and Spotlight then writes to it, so Burnout ejects the drive when
the check ends. Windows holds the drive offline after the write, so the same
place says that on Windows.

A Windows ISO takes the same command. Two options change what Setup does, and
Burnout writes nothing else into `autounattend.xml`:

- `--skip-hardware-checks` turns off the checks of Windows 11 for TPM, Secure
  Boot, RAM, CPU and storage.
- `--no-microsoft-account` takes away the step of the Microsoft account. Setup
  then asks for a local account and its password, so Burnout holds no
  password.

```bash
burnout write Win11_25H2_English_Arm64_v2.iso 3 --skip-hardware-checks --no-microsoft-account
```

Setup shows the editions in the image, and you choose one there.
The report counts the files, and names the check:

```
Wrote 963 files of 8.0 GB (7,988,543,418 bytes) to Samsung Flash Drive: 962 onto partition 1, FAT32, and 1 onto partition 2, exFAT.
Checked each file through a new mount of its volume against the SHA-256 that it went in with, and the partition table against the one that Burnout wrote.
macOS ejected the drive, so nothing mounts it or writes to it until you connect it again.
```

<details>
<summary><b>What happens inside a Windows write</b></summary>

```
burnout write Win11_25H2.iso 2
  -> reads the size that the ISO records, and refuses a file that is shorter
  -> reads the first 512 bytes, finds no hybrid table
  -> finds sources/install.wim, and selects Windows mode
  -> plans both partitions, and refuses a drive that is too small
  -> checks the privilege, and starts itself again through sudo if it must
  -> names the target drive, and waits for you to confirm it
  -> unmounts every volume on the drive
  -> writes an MBR with two partitions
  -> formats partition 1 as FAT32, and copies every boot file to it
  -> writes autounattend.xml onto partition 1
  -> writes partition 2 as exFAT, with the install image whole
  -> flushes the drive
  -> opens the drive again, and reads each file back against its hash
  -> ejects the drive on macOS
  -> reports what it checked
```

Burnout calls no tool of the host at any step above.
It writes the partition table and both file systems itself, which is the only
reason the steps are identical on three operating systems.

</details>

### A write that stops

Burnout resumes nothing, and it promises nothing about a write that stops.
It says what the drive holds, after an error and after Ctrl-C alike:

```
burnout: stopped. The write did not end, so the drive holds part of the image, and it is not usable now. Write the image again
```

A stop during the check says that the drive holds the whole image and that
Burnout did not complete the check. A stop before the write says that Burnout
wrote nothing. After a signal, the exit code is 128 and the number of the
signal, as a shell gives it, and 130 on Windows. A signal that the parent
ignores stays ignored, so a write under `nohup` goes on to its end.

### For a script

`--json` prints JSON on the output stream, and the text for a person goes to
the error stream. `list` prints one object:

```bash
burnout list --json
```

```
{"drives":[{"number":1,"id":"disk0","node":"/dev/rdisk0","name":"APPLE SSD AP0512Z",...,"system":true,"read_only":false},...],"system_disk_known":true}
```

`write` prints one object on each line. The confirmation still reads its
answer, so a script gives the answer that the confirm event names:

```bash
printf 'yes\n' | burnout write ubuntu-24.04.iso --device /dev/sdb --json
```

```
{"event":"confirm","image":{"path":"ubuntu-24.04.iso","size_bytes":2109796352},"mode":"raw","drive":{...},"expects":"yes"}
{"event":"start","stage":"unmount","total_bytes":null}
{"event":"done","stage":"unmount","bytes_done":0}
{"event":"start","stage":"write","total_bytes":2109796352}
{"event":"progress","stage":"write","bytes_done":536870912,"total_bytes":2109796352}
...
{"event":"result","mode":"raw","image_bytes":2109796352,"written_bytes":2109796352,"checked_bytes":2109796352,"sha256":"162ba3c5...","ejected":true,"offline":false}
```

An error prints `{"event":"error","kind":"in_use","message":"...","note":null}`,
and a stop prints `{"event":"stopped","reached":"writing","message":"..."}`.
Test the `kind`, and not the message: the message is for a person, and it can
change.

---

## Safety

> [!WARNING]
> **Burnout erases the drive that you give it.**
> The erased data goes nowhere, and no undo exists. A wrong target, a drive
> that fails during the write, and a cable that comes loose all give the same
> result, and none of them is recoverable. Keep a backup of anything you value.
> Read [TERMS.md](TERMS.md) section 4 before you run it.

What the code must never do, from
[CONTRIBUTING.md](CONTRIBUTING.md):

- Write to a device that the person did not select and confirm.
- Write to the disk that the running system starts from.
- Treat a fixed disk as a removable one.
- Report a verification pass that it did not run.
- Hide the target behind a default. The person names the target every time.

**You supply the operating system.**
Burnout downloads no operating system, hosts none, and gives you a licence for
none.
A Windows installation needs a licence from Microsoft.

---

## Project documents

| Document | What it holds |
| :-- | :-- |
| [docs/roadmap.md](docs/roadmap.md) | The order the work happens in, and the exit test for each phase |
| [docs/milestones.md](docs/milestones.md) | Every decision, the reason for it, and the options that lost |
| [docs/releasing.md](docs/releasing.md) | How a release is cut, and how to check a download |
| [CONTRIBUTING.md](CONTRIBUTING.md) | How to take part, and the rule that no test opens a real device |
| [AGENTS.md](AGENTS.md) | The rules for anybody who changes this repository, human or agent |
| [SECURITY.md](SECURITY.md) | The threat model, and how to report a vulnerability privately |
| [TERMS.md](TERMS.md) | What the tool does to your data, and what you agree to |
| [SUPPORT.md](SUPPORT.md) | Where to ask a question |
| [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) | Applies to everybody who takes part |

---

## What the spike proved

Both open questions are answered, against a running Windows 11 Setup.
[docs/spike-layout.md](docs/spike-layout.md) holds the evidence.

- **Windows PE reads exFAT.** It mounted the exFAT partition as `D:` and
  walked its directory tree. The layout above stands.
- **Setup does not find the install image across the partition boundary.**
  The volume was mounted and readable, and Setup still reported a missing
  media driver. So the `InstallFrom` path in `autounattend.xml` is a
  **requirement**, not insurance, and Burnout writes that file on every
  Windows drive whether or not anybody asks for an option.
- **The hardware checks come off from the same file.** Five `LabConfig`
  commands, and the refusal screen for TPM 2.0 and Secure Boot never appears.

One risk came out of it: the path holds a drive letter, and Windows PE assigns
letters itself. The milestones record what to do about that.

---

## Licence

MIT. See [LICENSE](LICENSE) and [TERMS.md](TERMS.md).

<div align="center">
<sub>Burnout erases drives. Read the warning above before you run it.</sub>
</div>
