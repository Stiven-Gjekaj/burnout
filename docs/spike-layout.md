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

### The trap in the harness

The first run sat on the firmware screen and never moved.
The signal that said so was not the screen, which can sit still for a good
reason.
It was the read counter: **42.5 GB read from a 12 GB disk in twelve minutes**,
which is the disk over three times, so the machine was reading in a loop and
not making progress.

The cause is the emulated processor.
UTM offered `qemu64`, and QEMU reports this for it:

| Model | sse4.1 | sse4.2 | popcnt |
| --- | --- | --- | --- |
| `qemu64` | no | no | no |
| `max` | yes | yes | yes |

Windows 11 24H2 and later refuse a processor with no SSE4.2 and no POPCNT.
The refusal arrives before anything can print, so it looks like a hang.

With `max`, the same drive read **626 MB and stopped**, which is `boot.wim`
read once into a RAM disk.
That is the shape of a healthy start.
The guest then ran with `RIP` inside `fffff800...`, which is kernel space, so
Windows PE was executing.

**Use `max`, and never `qemu64`, to test a Windows 11 image.**
Watch the read counter rather than the screen, because a screen that does not
change means nothing on its own.

### What is confirmed so far

The firmware starts the drive:

    BdsDxe: loading Boot0001 "UEFI QEMU QEMU USB HARDDRIVE 1-0000:00:02.0-4.1"
    BdsDxe: starting Boot0001 "UEFI QEMU QEMU USB HARDDRIVE 1-0000:00:02.0-4.1"

So UEFI firmware starts a FAT32 partition 1 on an MBR drive, which is the
first half of the layout.
The Windows boot manager then loads `boot.wim`.

### Where it stopped

**Question 2 is not answered, and the emulator is the reason.**

With `max` and the q35 machine, Windows PE loads `boot.wim` and starts the
kernel. It then stops. The instruction pointer oscillates between two
addresses 22 bytes apart, in one address space, with no disk read and no
repaint:

    RIP=fffff800a81449b2  <-> fffff800a814499c   (2 distinct values in 10 samples)
    RIP=fffff804dff449b2  <-> fffff804dff4499c   (a later boot, 2 in 8 samples)

The base address moves between boots because the kernel is relocated, and the
low digits do not, so both boots stop at **the same instruction in the same
function**. That is a deterministic hang and not a slow one.

Neither knob helped:

| Change | Result |
| --- | --- |
| 2 processors to 1 | The same two addresses. Not a multiprocessor start. |
| q35 to i440FX | Worse. 12,800 bytes read, and RIP stays in firmware. |

The layout is not the cause.
The firmware starts partition 1, the boot manager reads `boot.wim` once and
whole, and the kernel runs.
What fails is emulating a current Windows kernel on this host, and that is a
property of the emulator.

### What this spike did prove

- Windows PE carries the exFAT driver and registers it exactly as it registers
  NTFS. Question 1 is closed.
- UEFI firmware starts a FAT32 partition 1 on an **MBR** disk presented as a
  **USB** drive. The first half of the layout boots.
- `install.wim` in a current image is 7.06 GiB, and the image is UDF.

### How to finish it

Two ways, and both answer the same question.

1. **Real hardware.** Write this layout to a real USB drive and start a PC.
   It is the definitive test, it is needed before trusting the design, and it
   takes minutes rather than hours.
2. **An ARM64 Windows 11 image.** The host is Apple silicon, so an ARM64 guest
   runs native and needs no emulation. Setup decides where to find its image
   in code that does not depend on the processor, so the answer carries over.
   It needs a download that nobody has made yet.

Until one of these runs, treat the `InstallFrom` path in `autounattend.xml` as
required rather than as insurance, because that is the assumption that cannot
be wrong.
