<div align="center">
  <a href="../README.md"><b>Burnout</b></a>
</div>

# P0. The layout spike

[The roadmap](roadmap.md) holds two questions that gate every line of the
Windows path.
This file holds the answers and the evidence for them.

The image under test is `Win11_25H2_English_x64_v2.iso`, build 10.0.26100.

---

## What the ISO actually holds

Three numbers, measured and not assumed.

| Measured | Value |
| --- | --- |
| `sources/install.wim` | 7,578,075,168 bytes, which is **7.06 GiB** |
| The ISO file system | **UDF**, which macOS reports at the mount |
| `sources/boot.wim` | 614,809,152 bytes |

**The premise holds.**
`install.wim` is over the 4 GiB limit of FAT32 by a wide margin, so a single
FAT32 partition cannot carry this image without a split.

**The UDF finding confirms a risk in the roadmap.**
ISO 9660 keeps an extent length in 32 bits and so cannot address a file of
4 GiB or more.
This image is UDF for that reason.
An ISO 9660 reader alone cannot read a current Windows ISO, so P5 needs a UDF
reader as well.

---

## Question 1. Does Windows PE read exFAT?

**Yes. This needed no virtual machine.**

Windows Setup runs from image 2 of `boot.wim`. That image carries the driver:

    2/Windows/System32/drivers/exfat.sys     435,616 bytes

A file on disk is not a loaded driver, so the next question is whether the
system registers it. The `SYSTEM` hive of image 2, at
`ControlSet001\Services`, answers it:

| Service | Start | Type |
| --- | --- | --- |
| `exfat` | 3, DEMAND_START | 2, kernel driver |
| `fastfat` | 3, DEMAND_START | 2, kernel driver |
| `ntfs` | 3, DEMAND_START | 2, kernel driver |
| `udfs` | 4, DISABLED | 2, kernel driver |

`exfat` carries the same configuration as `ntfs` and `fastfat`.
A file system driver is demand start by design: the system loads it when a
volume of that type appears.
So Windows PE mounts an exFAT volume by the same path that it mounts NTFS.

**The layout in [the milestones](milestones.md) stands.**
Partition 2 stays exFAT, no file is split, and Burnout writes no WIM.

One aside worth keeping: `udfs` is **disabled** in the Setup image.
It costs Burnout nothing, because Burnout reads the ISO on the host and writes
FAT32 and exFAT to the drive.
It would matter to anybody who expects Windows PE to read a UDF volume.

### How to repeat this

    hdiutil attach -readonly -nobrowse Win11_25H2_English_x64_v2.iso
    7z l /Volumes/CCCOMA_X64FRE_EN-US_DV9/sources/boot.wim \
      | grep -E "System32/drivers/(exfat|udfs|fastfat|ntfs)\.sys$"
    7z e -o./hive /Volumes/.../sources/boot.wim "2/Windows/System32/config/SYSTEM"

Then read `ControlSet001\Services\<name>\Start` out of the hive.

---

## Question 2. Does Setup cross the partition boundary?

**Open. A boot answers this one, and nothing else does.**

### The drive under test

A 12 GB raw image, MBR, two partitions, built on macOS:

    Partition 1  FAT32  1.4 GB  every file of the ISO except install.wim
    Partition 2  exFAT  11.5 GB  sources/install.wim, whole

No `autounattend.xml` is present.
This is the strict test: Setup has to find the image by itself.
If it fails, the next run adds an `autounattend.xml` that names the image in
`<ImageInstall><OSImage><InstallFrom>`, and that tells us whether the path is
a requirement or an insurance.

### The harness

The drive boots in UTM as a USB hard drive on an emulated x86_64 machine with
UEFI firmware. The host is Apple silicon, so the processor is emulated and
every step is slow.

Watching it needed a detour worth writing down.
UTM runs QEMU in a sandbox that refuses a file write, so the QMP `screendump`
command cannot save a frame anywhere on the disk.
QEMU accepts a VNC head next to the SPICE display that UTM itself uses, so a
small client asks for one raw frame and writes a PNG.
That reads the screen with no access to the desktop of the person running it.

### What is confirmed so far

The firmware starts the drive:

    BdsDxe: loading Boot0001 "UEFI QEMU QEMU USB HARDDRIVE 1-0000:00:02.0-4.1"
    BdsDxe: starting Boot0001 "UEFI QEMU QEMU USB HARDDRIVE 1-0000:00:02.0-4.1"

So UEFI firmware starts a FAT32 partition 1 on an MBR drive, which is the
first half of the layout.
The Windows boot manager then loads `boot.wim`.

**This result is not in yet.**
