<div align="center">
  <a href="../README.md"><b>Burnout</b></a>
</div>

# Milestones

Nothing is built yet.
This file holds the decisions that shape the work, and the reason behind each
one.
It also holds the options that lost, because the reason a choice lost is the
part that a later reader needs.

Nobody has run any of this.
Two assumptions carry the whole Windows path, and they are named at the end.
Test them before you write real code.

---

## The rule that decides everything else

**Burnout never delegates to the host.**

It calls no `diskutil`, no `mkfs`, no `format`, and no `bootsect`.
It mounts nothing.
It opens the raw device, and it writes the partition table and the file
systems itself.

The goal is one process that behaves the same way on Windows, on macOS, and on
Linux.
The moment the code calls `diskutil` on one host and `mkfs.vfat` on another,
it inherits three sets of defaults, three sets of failure messages, and three
sets of bugs.
"The same process" then becomes a thing to maintain instead of a thing that is
true.

The rule also buys the tests.
Code that writes to a seekable target works against a file, so almost all of
Burnout runs in a test that touches no device.
[CONTRIBUTING.md](../CONTRIBUTING.md) refuses a test that opens a real device,
and this rule is what makes that refusal practical.

The operating system layer holds five operations only:

- List the drives.
- Unmount the volumes of a drive.
- Open the device.
- Flush.
- Close.

Everything above that layer is one body of code.

---

## Two write modes

A drive gets one of two treatments.
The modes share the device layer, the progress reporting, and the safety
checks, and they share nothing else.

| | Raw mode | Windows mode |
| --- | --- | --- |
| For | Linux, BSD, any hybrid ISO, any `.img` | A Windows installer ISO |
| What it does | Copies the image byte for byte | Partitions, formats, writes files |
| The partition table | Comes from inside the image | Burnout creates it |
| Verification | Hash of the device against the source | A hash for each file |

A Linux ISO is already a bootable disk image.
It carries its own boot code, its own partition table, and its own EFI system
partition inside the file.
Writing it means a copy of the bytes, and nothing else.

A Windows ISO is not a bootable disk image.
It has no boot code for a USB drive.
A byte for byte copy of it gives a drive that most firmware refuses.
That difference is the reason Windows mode exists, and the reason a tool like
Rufus exists at all.

Raw mode is the first thing to build.
It is one code path, it exercises the whole device layer on all three hosts,
and it gives a useful tool before any Windows work starts.

---

## Burnout chooses the mode, and the person does not

Read the first 512 bytes of the image.

- A boot signature and a partition table there mean a hybrid image.
  Use raw mode.
- No hybrid table, and `sources/install.wim` or `sources/install.esd` inside,
  mean a Windows installer.
  Use Windows mode.

`--mode raw|windows` overrides the result.
It exists for the day the guess is wrong, and a normal person never types it.

This is the general rule for the whole command line.
An option that the tool can decide for itself is a bug in the tool, not a
choice for the person.

---

## macOS install media is a non-goal

Linux and BSD ship a hybrid ISO, which is already a bootable disk image.
macOS is a third category, and it fits neither mode.
Burnout does not make a macOS installer, and it is not a thing to add later.

**Apple ships no ISO.**
You get `Install macOS <name>.app` from the App Store, or an
`InstallAssistant.pkg`.
Neither one is a disk image.

**A `.dmg` file is usually not a raw image.**
Apple compresses them, so a byte for byte copy gives a drive that starts
nothing.
An uncompressed read and write dmg is a real raw image, and raw mode writes one
like any other.

**The supported path is `createinstallmedia`.**
It is a closed binary inside the `.app`, it runs on macOS only, and it needs
root.
A call to it breaks the rule at the top of this file, and it does not exist on
Windows or on Linux, so the promise of one process on three hosts ends there.

**The file system is HFS+ or APFS.**
APFS has no complete public specification.
That is a different order of work from the exFAT writer, and it is not work
this project takes on.

**Apple silicon adds boot rules of its own**, and no tool can make media for a
Mac that is newer than the installer.

### What the code does instead

Detect the input and refuse with a sentence that helps.

- An `.app` bundle, an `InstallAssistant.pkg`, or a compressed dmg: stop, say
  that Burnout does not make macOS install media, and name
  `createinstallmedia`.
- An uncompressed raw dmg: treat it as any other raw image.

A clear refusal costs one message.
A drive that starts nothing costs an hour, and the person does not know why.

---

## The layout that Windows mode writes

One MBR partition table, and two partitions.

- **Partition 1. FAT32, about 1 GB.** Every file from the ISO except
  `sources/install.wim` or `sources/install.esd`.
- **Partition 2. exFAT, the rest of the drive.** The install image, whole.

UEFI firmware reads FAT only, so every boot file sits on partition 1, and the
firmware never looks at partition 2.
Windows PE starts from partition 1 and then reads partition 2 itself.

MBR, and not GPT.
Burnout writes the table itself, so the spare EFI partition that `diskutil`
adds on macOS never appears.
MBR also leaves the door open for Legacy BIOS boot later, and that costs
nothing now.

### Why the install image does not get split

FAT32 holds no file of 4 GiB or more, and a current `install.wim` is usually
larger.
The usual answer is to split it into `install.swm` parts.
WinDiskWriterX does this, and it uses wimlib for the work.

Burnout writes no WIM file at all.
The second partition has no 4 GiB limit, so the image goes on whole.
This also handles `install.esd`, which the split approach never did.

The consequence is the licence.
wimlib is LGPL, and it is about seventy thousand lines of C to link on three
operating systems.
Burnout stays MIT, and the build stays simple, because it never needs wimlib.

### Why partition 2 is exFAT and not NTFS

NTFS was the first choice, and it was wrong.

**macOS cannot write NTFS.**
It mounts NTFS read only.
An NTFS partition gives a Windows path that works on two hosts out of three,
which defeats the point of the project.

exFAT has no 4 GiB file limit either, and Windows PE reads it.
Burnout writes the file system itself, so the host does not need to support
exFAT for anything.
Nothing mounts the drive on the machine that makes it.

---

## The Windows 11 options come from one text file

Windows Setup reads `autounattend.xml` from the root of the drive.
Burnout writes that file, and nothing else, to give the options that people
want:

- **Skip the hardware checks.** A `RunSynchronous` command in the `windowsPE`
  pass adds the `LabConfig` keys for the TPM, Secure Boot, RAM, storage and
  CPU checks.
- **Skip the Microsoft account.** A local account in `<UserAccounts>`.
  WinDiskWriterX lists this as a thing it does not do.
- **Name the install source.** `<ImageInstall><OSImage><InstallFrom>` points
  Setup at the image on partition 2. **This one is not optional.**

**Burnout writes this file on every Windows drive, whether anybody asks for an
option or not.**
[The spike](spike-layout.md) measured why.
Windows PE mounts the exFAT partition and reads it, and Setup still does not
look there, because Setup looks beside `setup.exe`.
With no `InstallFrom` path, Setup stops and says that a media driver is
missing.
So the file that carries the options is also the file that makes the layout
work at all.

Keep the rest of it small.
Write only the entries that the person asks for, and leave the rest of Setup
interactive.

The path holds a drive letter that Windows PE assigns for itself, and a
machine with another disk attached numbers things differently.
[The spike](spike-layout.md) measured that, and it closed it.
A fixed letter fails with `0x80070490` as soon as a second disk exists, so the
file carries a `RunSynchronous` search that finds the volume holding
`sources\install.wim` and pins it to `W:` with `diskpart`, and the path then
names `W:`.
`diskpart` cannot select a volume by label, so the search is by content and
not by name.

Two options lost, and both for the same reason.

- **An offline edit of the SYSTEM hive inside `boot.wim`.** This needs WIM
  read, WIM write, recompression, and a registry hive writer.
  It is the most work of any option by a wide margin.
- **The Server installation type.** WinDiskWriterX sets `installationtype` to
  Server in the image metadata, and Setup then skips the checks.
  It is clever and much cheaper than the hive edit, but it is still a write to
  a WIM file, and the rule above says Burnout writes none.

Keep the Server trick in mind as a fallback for somebody who wants no
`autounattend.xml` on the drive at all.

A text file has one more property that the other two do not.
The person can read it, change it, or delete it, and the drive is then stock.

---

## Elevation

A raw device write needs root on macOS and on Linux, and Administrator on
Windows.
Burnout cannot avoid this, and it does not try.

**Burnout never asks for a password itself.**
On macOS and on Linux it starts itself again through `sudo`, and `sudo` asks.
The password goes to a program that already holds that trust.
A tool that reads a password into its own memory and passes it on is how this
kind of tool gets a vulnerability.

Three details in the restart:

- Use the path from `current_exe()`, and never `argv[0]`. The caller controls
  `argv[0]`.
- Put `--` before the path, or `sudo` reads the Burnout options as its own.
- Pass a list of arguments, and never a command string. There is then no
  quoting and no injection.

Set `BURNOUT_ELEVATED=1` before the restart, and refuse to elevate a second
time.
Without that guard, any case where elevation appears to succeed while the
user id stays wrong asks for a password forever.

Try `sudo` first, then `doas`.
Do not use `pkexec`, which targets a desktop and changes the environment.

**On Windows, do not elevate.**
Detect the shortfall and print how to fix it.
Windows can elevate, through `ShellExecuteEx` with the `runas` verb, and the
result is worse than a clean failure: it starts a second process in a **new
console window**, the first terminal returns at once, the progress goes to a
window that nobody watches, that window closes at the end, and the exit code
never reaches the shell.

### Elevate at the right moment

    parse the arguments            no privilege
    validate the image file        no privilege
    list the drives, find the target   no privilege
    ------------------------------------------------
    check the privilege, elevate here
    ------------------------------------------------
    confirm the target with the person
    write

`burnout list` runs with no privilege on all three hosts, so a person sees
their drives without a password.
Elevation comes after the image check, so nobody gives a password and then
learns that the path is wrong.
It comes before the confirmation, so the person confirms one time and not one
time for each process.

`--no-elevate` fails instead of asking, for a script.
When the input is not a terminal, do not try to elevate at all.
A `sudo` prompt inside a pipeline stops and waits for nobody.

### Build no helper

Do not ship a setuid binary, and do not ship a privileged background helper.
Either one lets any local user drive Burnout into a disk that the user cannot
open alone.
[SECURITY.md](../SECURITY.md) lists that as in scope.
The restart through `sudo` is safe because it leaves the decision with the
operating system.

---

## The shape of the command

A device name is the one thing that cannot be the same on three hosts.
`/dev/disk4`, `/dev/sdb` and `\\.\PhysicalDrive2` have nothing in common.

So a device path is never the target.

    burnout list
    burnout write ubuntu.iso 2

`list` prints a stable index, the maker, the size, and the connection.
The two commands above are the same on all three hosts.

`--device /dev/sdb` stays available for a script.

---

## Verification makes two different promises

Say which one ran.
Do not let one word cover both.

- **Raw mode** compares a hash of the written device against a hash of the
  source file. This proves the drive holds the image.
- **Windows mode** built a file system that the source never had. A hash of
  the whole device proves nothing there. The check is a hash for each file
  that Burnout copied.

[SECURITY.md](../SECURITY.md) puts a verification pass that reports a success
it did not run in scope, so the report must name the check it performed.

---

## Both of these are now measured

[The spike](spike-layout.md) answered them on a running Windows 11 Setup.

1. **Windows PE reads exFAT.** It mounted the exFAT partition as `D:` and
   walked its directory tree. The layout stands, and no file is split.
2. **Setup does not find the image across the partition boundary.** The
   `InstallFrom` path is a requirement, not an insurance.

The `LabConfig` keys were measured at the same time, and they work: the screen
that refuses a machine with no TPM 2.0 never appears.

---

## Version 1

| In | Out |
| --- | --- |
| Raw mode for a hybrid image | Legacy BIOS boot |
| Windows mode, UEFI boot | Windows To Go |
| The mode chosen from the image | A write to one partition |
| `list` and `write` | A drive that holds many images |
| Refusing a macOS installer clearly | macOS install media, ever |
| The `autounattend.xml` options | A graphical interface |
| Verification, named per mode | |
| Elevation on all three hosts | |

**Legacy BIOS waits.**
It costs a grub4dos download at run time, MBR and partition boot code, and a
test on hardware that gets rarer each year.
The MBR layout above means it fits later with no change to anything.

**Windows To Go waits.**
It applies an image to a disk instead of copying an installer, so it is a
third write mode and not an option on the second one.

**macOS install media does not wait.**
It is a non-goal, and the section above says why.

---

## The toolchain

Rust.
It gives one static binary for each host with no runtime, and the three device
layers are small enough to write directly against `SetupAPI` and the IO
control codes on Windows, IOKit and Disk Arbitration on macOS, and `libudev`
and `sysfs` on Linux.

- `clap` for the command line.
- `indicatif` for progress.
- `fatfs` for FAT32. It is MIT, and it works over anything that seeks, so it
  works over a test file as well as a device.
- `blake3` for the hashes.
- An exFAT writer, which this project writes. The format is documented, and it
  is a few thousand lines. It is far less work than NTFS, and less work than a
  WIM splitter.

Add a dependency only with a reason in the pull request.
Prefer MIT and Apache 2.0.
A copyleft dependency is not forbidden, and it does change what this project
can ship in one binary, so say so.

### One correction to carry over

WinDiskWriterX asks for Administrator rights only for Legacy BIOS boot,
because `diskutil` does the rest of the work.
Burnout partitions the raw device itself, so it needs root or Administrator on
every path, every time.
