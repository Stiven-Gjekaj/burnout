<div align="center">
  <a href="../README.md"><b>Burnout</b></a>
</div>

# P0. The layout spike

[The roadmap](roadmap.md) held two questions that gate every line of the
Windows path.
**Both are answered.**
One of the answers is the opposite of what the plan assumed.

Images under test: `Win11_25H2_English_x64_v2.iso` and
`Win11_25H2_English_Arm64_v2.iso`, build 10.0.26100.
The SHA256 of the ARM64 image matches the value Microsoft publishes.

---

## The short version

| Question | Answer |
| --- | --- |
| Does Windows PE read exFAT? | **Yes.** Mounted as `D:` and browsable. |
| Does Setup find the image across the partition boundary? | **No.** |
| Is `InstallFrom` insurance or a requirement? | **A requirement.** |
| Does `LabConfig` through `autounattend.xml` work? | **Yes.** |

The layout stands. `autounattend.xml` stops being optional.

---

## What the ISO holds

| Measured | x64 image | ARM64 image |
| --- | --- | --- |
| `sources/install.wim` | 7,578,075,168 bytes, **7.06 GiB** | 7,075,454,641 bytes, **6.59 GiB** |
| The ISO file system | **UDF** | **UDF** |

**The premise holds.**
Both install images are far over the 4 GiB limit of FAT32.

**The UDF finding confirms a risk in the roadmap.**
ISO 9660 keeps an extent length in 32 bits and cannot address a file of 4 GiB
or more, so a current Windows ISO is UDF.
An ISO 9660 reader alone cannot read one, and P5 needs a UDF reader.

---

## Question 1. Does Windows PE read exFAT?

**Yes.** Two independent checks agree.

### From the image, with no machine running

Setup runs from image 2 of `boot.wim`, and that image carries the driver:

    2/Windows/System32/drivers/exfat.sys     435,616 bytes

A file on disk is not a loaded driver. The `SYSTEM` hive of image 2, under
`ControlSet001\Services`, settles it:

| Service | Start | Type |
| --- | --- | --- |
| `exfat` | 3, DEMAND_START | 2, kernel driver |
| `fastfat` | 3, DEMAND_START | 2, kernel driver |
| `ntfs` | 3, DEMAND_START | 2, kernel driver |
| `udfs` | 4, DISABLED | 2, kernel driver |

`exfat` carries the configuration of `ntfs` and `fastfat`.
Demand start is correct for a file system driver: the system loads it when a
volume of that type appears.

One aside: `udfs` is **disabled** in the Setup image.
It costs Burnout nothing, because Burnout reads the ISO on the host.

### From a running Setup

The Browse dialog of Setup listed the volumes it had mounted:

    This PC
      BOOT (C:)        the FAT32 partition
      INSTALL (D:)     the exFAT partition
        sources        the directory, which opens
      Boot (X:)        the Windows PE RAM disk

Windows PE mounted the exFAT partition, gave it a letter, and walked its
directory tree.

---

## Question 2. Does Setup cross the partition boundary?

**No. `InstallFrom` is required, and not insurance.**

This is the result that matters most, because the plan assumed the opposite.

### Run 1, with no `autounattend.xml`

Setup stopped at **"Install driver to show hardware"**, which reads "A media
driver your computer needs is missing".
That is what Setup says when it cannot find the install image.

At that moment the exFAT volume was mounted as `D:`, readable, and held
`sources\install.wim`, as the Browse tree above shows.
Setup still did not find it.
Setup looks beside `setup.exe`, which sits on the FAT32 partition, and stops
there.

### Run 2, with the path named

One file at the root of the FAT32 partition:

    <InstallFrom>
      <Path>D:\sources\install.wim</Path>
      <MetaData wcm:action="add">
        <Key>/IMAGE/INDEX</Key><Value>1</Value>
      </MetaData>
    </InstallFrom>

Setup went language, keyboard, product key, and on to the hardware check.
The media error never appeared.
No edition list appeared either, because the index chose the edition **out of
the image on the exFAT partition**.

### Run 3, with the hardware checks turned off

Five `RunSynchronous` commands in the `windowsPE` pass:

    reg add HKLM\SYSTEM\Setup\LabConfig /v BypassTPMCheck /t REG_DWORD /d 1 /f
    ...and BypassSecureBootCheck, BypassRAMCheck, BypassCPUCheck, BypassStorageCheck

Run 2 stopped at **"This PC doesn't currently meet Windows 11 system
requirements"**, naming TPM 2.0 and Secure Boot.
Run 3 went from the product key straight to the licence terms.
The refusal screen never appeared.

---

## What this changes

**`autounattend.xml` stops being optional.**
[The milestones](milestones.md) describe it as the file that carries the
options a person asks for.
It is now also the file that makes the two-partition layout work at all.
Burnout writes it on every Windows drive, whether or not anybody asks for an
option.

### The risk this opened, and how it closes

A second spike ran with a second disk attached, because a real PC has one.
The disk was 20 GB with one exFAT partition labelled EXISTING, on NVMe, and
the test drive stayed on USB.

**The letters move.** `diskpart` in Windows PE reported:

| Volume | Letter | Label | File system |
| --- | --- | --- | --- |
| 0 | **C** | EXISTING | exFAT |
| 1 | **D** | BOOT | FAT32 |
| 2 | **E** | INSTALL | exFAT |

With one disk the install partition was `D:`. With two it is `E:`, and `D:` is
now the boot partition.
Setup failed at once with **`0x80070490`**, which is `ERROR_NOT_FOUND`: the
path in the unattend pointed at a partition that holds no install image.

**A fixed drive letter in the unattend file is unusable.** A drive built that
way works on a bare test machine and fails on every real computer that has a
disk in it.

#### What does not work

`diskpart` cannot select a volume by its label:

    DISKPART> select volume=INSTALL
    The arguments specified for this command are not valid.

It takes a volume number or a drive letter, and both of those move.

#### What does work

`diskpart` reassigns a letter once a volume is selected by its current letter:

    DISKPART> select volume=E
    Volume 2 is the selected volume.
    DISKPART> assign letter=W
    DiskPart successfully assigned the drive letter or mount point.

So the unattend file finds the volume by looking for the image on it, and then
pins that volume to a letter Burnout chose:

    cmd /c for %d in (C D E F G H I J K L M N O P Q R S T U V Y Z) do
      @if exist %d:\sources\install.wim
        ((echo select volume=%d&echo assign letter=W)>X:\bo.txt&diskpart /s X:\bo.txt)

`<InstallFrom><Path>` then names `W:\sources\install.wim`, which is a letter
that nothing else holds.
The search cannot pick the boot partition by mistake, because the boot
partition carries `sources\boot.wim` and not `sources\install.wim`.

**Measured with the second disk attached:** Setup started with no error, took
the language, keyboard and product key screens, and reached the licence terms
with the edition already chosen out of the image.
The hardware refusal screen did not appear either.

**So the layout survives.** It needs three things in the unattend file, not
one: the search that pins the letter, the `InstallFrom` path that uses it, and
the `LabConfig` keys.

## The harness

### The emulated attempt failed, and the reason is worth keeping

The x86_64 image cannot run on Apple silicon without emulation, and Windows 11
did not survive it.

The first run looked like a hang.
The signal that said otherwise was not the screen, which can sit still for a
good reason.
It was the read counter: **42.5 GB read from a 12 GB disk in twelve minutes**,
which is the disk over three times, so the machine was looping.

The cause was the emulated processor:

| Model | sse4.1 | sse4.2 | popcnt |
| --- | --- | --- | --- |
| `qemu64` | no | no | no |
| `max` | yes | yes | yes |

Windows 11 24H2 and later refuse a processor with no SSE4.2 and no POPCNT, and
the refusal arrives before anything can print.

With `max` the disk read **626 MB once and stopped**, which is `boot.wim` read
into a RAM disk, and the kernel started.
It then stopped for good: `RIP` oscillated between two addresses 22 bytes
apart, in one address space, with no disk read and no repaint.
Across two boots the kernel base moved and the low digits did not, so both
stopped at the same instruction.
One processor instead of two made no difference, and i440FX instead of q35 was
worse.

**Do not test a current Windows image under emulation.** Use an image that
matches the host and run it on the hardware.

### The harness that worked

    Architecture aarch64, machine virt, Hypervisor on, UEFI on
    One USB disk: MBR, FAT32 1.4 GB boot files, exFAT 11 GB install.wim

Windows Setup appeared **45 seconds** after start.

### Watching it

UTM runs QEMU in a sandbox that refuses a file write, so the QMP `screendump`
command cannot save a frame anywhere.
QEMU accepts a VNC head next to the SPICE display that UTM uses, so a small
RFB client asks for one raw frame and writes a PNG, and QMP `input-send-event`
moves the pointer and clicks.
That drives the machine without touching the desktop of the person running it.

### Repeating it

    scripts/spike-drive.sh <windows.iso> <out.img> [autounattend.xml]

The script refuses to run unless `diskutil` reports the target is virtual, so
it cannot write a real disk.
