<div align="center">
  <a href="../README.md"><b>Burnout</b></a>
</div>

# The roadmap to version 1

[docs/milestones.md](milestones.md) says what Burnout does and why.
This file says in what order to build it.

Each phase has a goal, the work in it, and an exit test.
Do not start a phase until the phase it depends on passes its exit test.
The sizes are relative: S is a day or two, M is a week, L is longer, and XL is
the one that needs a plan of its own.

P0 to P4 are done. Raw mode writes a drive on all three hosts.
The layout of Windows mode, the table and both of its file systems, passes
the checks of each host, and the command line does not use it yet. P6 joins
it to the command line.

---

## The order, and the reason for it

Two rules set the order.

**Risk first.**
The layout that Windows mode writes rested on two claims that nobody had
tested.
If they were wrong, the layout changed, and any Windows code written before
then was wasted.
So the test came before the code, and it needed no code. P0 ran it.

**Useful early.**
Raw mode is one code path, it exercises the whole device layer on three hosts,
and it writes a Linux drive.
That is a tool somebody can use, and it arrives long before Windows mode.
Ship it.

```mermaid
flowchart LR
    P0([P0 prove the layout]) -.gates.-> P6
    P1[P1 device layer] --> P2[P2 raw mode]
    P2 --> P3[P3 MBR and FAT32]
    P3 --> P4[P4 exFAT]
    P4 --> P6[P6 Windows mode]
    P5[P5 read an ISO] --> P6
    P6 --> P7[P7 the edges]
```

P0 and P5 do not wait for anything.
Start P0 now, because it can send P3 and P4 back to the drawing board.

| | Phase | Size | Release |
| --- | --- | --- | --- |
| P0 | Prove the layout | S | **done** |
| P1 | The skeleton and the device layer | L | **done** |
| P2 | Raw mode | M | **v0.1** |
| P3 | The partition table and FAT32 | M | **done** |
| P4 | The exFAT writer | XL | **v0.2** |
| P5 | Reading an ISO | L | |
| P6 | Windows mode | L | **v0.3** |
| P7 | The edges | M | **v1.0** |

---

## P0. Prove the layout

**Size S. Done. [The result is here](spike-layout.md).**

Both questions are answered. Windows PE reads exFAT, and Setup does **not**
find the image across the partition boundary, so `autounattend.xml` with an
`InstallFrom` path is required on every Windows drive.

Build the two-partition drive by hand, with whatever tools your machine has.
This is an experiment and not a product, so `diskutil`, `mkfs` and `dd` are
all allowed here.
They are not allowed in the code.

The work:

1. Make an MBR drive with a FAT32 partition of about 1 GB and an exFAT
   partition for the rest.
2. Copy every file of a Windows 11 ISO to partition 1, except
   `sources/install.wim`.
3. Copy `sources/install.wim` to partition 2, keeping the `sources` directory.
4. Start it on real firmware in UEFI mode.
5. Repeat with an `autounattend.xml` that names the image in
   `<ImageInstall><OSImage><InstallFrom>`, and again without it.

**The exit test.** Two questions have written answers in
`docs/spike-layout.md`:

- Does Windows PE read exFAT?
- Does Setup find the install image on the second partition, with the
  `InstallFrom` path and without it?

**If exFAT fails**, the fallback is FAT32 on both partitions and a WIM splitter
written in Rust.
That keeps the MIT licence, it does not solve an oversized `install.esd`, and
it adds a phase the size of P4.
Better to learn it in an afternoon than in month three.

---

## P1. The skeleton and the device layer

**Size L. Done.**

`burnout list` names every drive on Windows, on macOS and on Linux, gives the
size to the byte, and marks the drive that the running system starts from. It
needs no privilege. `burnout-core` carries no dependency at all.

The five operations that the operating system owns, and nothing above them.

The work:

- A Cargo workspace. `burnout-core` holds everything that is not
  host-specific, and takes any target that reads, writes and seeks, so a test
  gives it a file. `burnout` is the command line on top.
- The device layer, in two traits and one handle. `DriveList` lists the
  drives and needs no drive and no handle. `DriveAccess` unmounts the volumes
  of one drive and opens it. The open handle reads, writes, seeks and flushes.
  Close is the drop of the handle.
  One trait cannot hold all five, because `flush` belongs to an open handle
  and `list` belongs to the host. One trait would make the write path of P2
  take the host as well as the target, and
  [CONTRIBUTING.md](../CONTRIBUTING.md) says the write path takes a source, a
  target handle and a progress callback, and nothing else.
- Linux: read `/sys/block` for the size, the model, the vendor and the
  removable flag. Unmount through the `umount2` system call.
  `umount` is a program, and this file says below that a tool of the host is
  not allowed. `umount2` is the peer of `DADiskUnmount` and
  `FSCTL_DISMOUNT_VOLUME`: three calls, and no subprocess. It also returns an
  error number rather than a sentence in the language of the host.
- macOS: IOKit for the list, Disk Arbitration for the unmount. Write to
  `/dev/rdiskN` and never `/dev/diskN`, because the raw node skips the buffer
  cache and is about ten times faster.
- Windows: SetupAPI for the list, `IOCTL_STORAGE_QUERY_PROPERTY` for the
  details, `FSCTL_LOCK_VOLUME` and `FSCTL_DISMOUNT_VOLUME` before the write.
- Find the disk the running system starts from, on each host, and mark it.
- Handle a logical sector size of 4096 as well as 512. A raw device refuses a
  write that its sector size does not divide.
- `burnout list`, with a stable index.
- GitHub Actions on Windows, macOS and Ubuntu, from the first commit.

An operating system API is not a tool of the host.
IOKit, Disk Arbitration and SetupAPI are how you open a device, so they are
fine.
`diskutil`, `mkfs` and `format` do the work that Burnout must do itself, so
they are not.

**The exit test.** `burnout list` names every drive correctly on all three
hosts, gives the size to the byte, and marks the system disk. It runs with no
privilege. CI is green on all three.

**Two claims that CI cannot answer.** Both are closed by hand.

*No privilege on Windows.* The GitHub runner signs in as an Administrator, so
a pass there says nothing. A clean Windows 11 ARM64 install in a virtual
machine carries a standard account, which is not in Administrators, holds the
Medium integrity level `S-1-16-8192`, and gets `Access is denied` from
`net session`. That account ran the list:

    1  QEMU NVMe Ctrl  25.8 GB (25,769,803,776 bytes)  NVMe  no   (system disk)
    2  QEMU HARDDISK   12.9 GB (12,884,901,888 bytes)  USB   yes

The two image files behind those drives are 25,769,803,776 and
12,884,901,888 bytes on the host, so both sizes are correct to the byte, the
bus is right for each, and the disk the machine started from carries the mark.

The run also found a fault that no host here could show: the binary linked the
Microsoft C runtime and stopped before it ran one line. The build now links
that runtime in.

*A 4Kn drive.* No drive here reports a 4096-byte sector, so `scsi_debug` made
one on Fedora ARM64:

    modprobe scsi_debug sector_size=4096 dev_size_mb=64

`/sys/block/sdb/size` read 131072 and `queue/logical_block_size` read 4096.
The list reported 67,108,864 bytes, which is 131072 x 512. The trap below
gives 536,870,912, which is eight times too large. The user id was 1000, so
this also shows the list needs no privilege on Linux.

---

## P2. Raw mode

**Size M. Depends on P1. Done. Ships as v0.1.**

**v0.1 needs the release workflow, so that work moves here from P7.**
The decision below puts build provenance on every release artifact, and P7
scheduled the workflow that makes it. A release at v0.1 cannot satisfy both,
and a first release without provenance would break the promise on the first
day it applies. The workflow builds each host, publishes a SHA256 for each
binary, attests each one, and opens a draft release that a person publishes.

The first thing a person can use.

The work:

- Read the image, write it to the device, in aligned blocks.
- Progress that reports what the code proves, and completion only after the
  flush returns. A byte count that reached the size of the source is not a
  finished write, because the operating system still holds a cache.
- Verify: read the device back and compare a hash against the source.
- The confirmation prompt, and the refusals: no system disk ever, no fixed
  disk unless forced, and no target that the person did not name.
- `--force` for a fixed disk. The person types the model and the size of the
  drive back, and not a letter and not the word yes. A prompt that takes one
  keystroke is one that people learn to answer without reading.
- Elevation. Restart through `sudo`, with the `BURNOUT_ELEVATED` guard against
  an endless loop. On Windows, print how to fix it and stop.
- `--no-elevate`, and no elevation attempt when the input is not a terminal.

**The exit test.** A drive written on each of the three hosts starts Ubuntu on
real hardware. The hash of the device matches the hash of the ISO. Every test
in the suite runs against a file, and none opens a device.

**It passed in virtual machines first.**

A 2,689,781,760 byte Fedora ARM64 image went onto a drive from each host, and
each of the three drives then started Fedora in a machine that had that drive
and nothing else. No install medium, no second disk.

| Host | The drive it wrote | What it printed |
| --- | --- | --- |
| macOS 26 | a 4 GB image attached with `hdiutil` | `162ba3c5...999933ef` |
| Fedora 44 ARM64 | a 3 GB virtual USB disk | `162ba3c5...999933ef` |
| Windows 11 ARM64 | a 3 GB virtual USB disk | `162ba3c5...999933ef` |

One digest from three hosts, and it is the digest that `shasum -a 256` gives
for the file on the host.

The Linux run also shows the restart, in the title of its window:

    sudo -- /tmp/b write /dev/sr0 2 --elevated

Two dashes before the path, the arguments carried across, and the guard added
on the end.

**Three things the first run did not touch, now run.**

*An unmount that had something to unmount.* The first run wrote to blank
disks, so the unmount had nothing to do on any host. Each host now writes over
a drive that the host itself had mounted.

| Host | Before the write | After it |
| --- | --- | --- |
| macOS | `/Volumes/TESTVOL`, and `/dev/rdisk5` answered `Resource busy` | the volume is gone and the write verified |
| Linux | `/dev/sda1` at `/run/media/liveuser/XFER` | the write verified |
| Windows | `G:` labelled TESTOL, with a file on it | the write verified |

*The write path at a 4096-byte sector.* `scsi_debug sector_size=4096` makes a
drive that refuses a write the sector size does not divide. An image of
5,000,003 bytes, which 4096 does not divide, went onto it whole and read back
with the digest that `sha256sum` gives for the file. That is the padding
proved against a device and not against a test double.

*The options nobody had typed.* `--force` prints the model and the size, takes
them back in any case and spacing, and refuses one wrong digit. A pipe with no
terminal behind it refuses to ask for a password and says to use `sudo`. Half
a yes is not a yes. Each refusal leaves the drive alone and exits 1.

**What a virtual drive still does not prove.** A QEMU USB disk is not a USB
stick. It has no controller of its own, it never fails in the middle of a
write, and it arrives with no descriptors of its own. The other half of this
exit test needs a stick that can be erased.

**The real stick, on one Mac.** A Samsung Flash Drive of 128.3 GB, with a
USB 3 controller of its own. An Apple M5 cannot start a Linux stick, so UTM
passed the stick itself through to a VM, and a VM start counts as the start on
real hardware. That leaves the firmware of a physical PC unproved, and the list
under "Not run yet" below has the other gaps.

| Host | How it reached the stick | What it printed |
| --- | --- | --- |
| macOS 26 | `/dev/rdisk5`, 55 MB/s, read back at 125 MB/s | `162ba3c5...999933ef` |
| Fedora 44 ARM64, a VM | the stick passed through, `/dev/sdb`, 48 MB/s, read back at 88 MB/s | `162ba3c5...999933ef` |
| Windows 11 ARM64, a VM | the stick passed through, `\\.\PhysicalDrive2`, 37 MB/s, read back at 87 MB/s | `162ba3c5...999933ef` |

The same digest as the ISO, from each of the three hosts. The stick that
macOS wrote then started Fedora in a VM that had no disk of its own, and so did
the stick that Windows wrote. The boot order of that VM named no disk, so its
firmware stopped at the UEFI shell, and the shell started
`\EFI\BOOT\BOOTAA64.EFI` from the stick. The media check of the initramfs,
`checkisomd5`, then read the image back from the first byte of the stick and
passed each time. That is a check of the write that Burnout did not make.

**Three faults that only the real stick found.**

- **Linux refused the stick as the system disk.** The VM started from its
  disc, and the stick in the port held the same image, which the desktop had
  mounted. The live-USB rule found the image on one drive, because it did not
  count the disc, and no option overrides that refusal. It now counts the disc,
  so a stick and a disc that hold the same image mark neither, and the write
  asks for the model and the size.
- **The rate of a write divided the bytes by the flush alone.** Linux takes
  the image into its page cache first, and the write and the flush both said
  303.1 MB/s for a stick that reads back at 27.1 MB/s. Each step now keeps its
  own clock, and the flush carries no rate. On macOS the same fault showed as
  a write with no rate at all.
- **`--device /dev/disk5` named no drive on macOS,** because Burnout writes
  `/dev/rdisk5`. The name under `/dev` now counts.

On Linux the stick answered for itself as well. After the write, the guest
dropped its caches and read the first 2,689,781,760 bytes of `/dev/sdb` in
22.1 seconds, and `sha256sum` gave the digest above. That read came off the
drive and not out of a cache.

**Four faults that only Windows and the real stick found.**

- **Windows refused the first write with "Incorrect function".** Partition 1
  of the Fedora image has the GPT attribute read-only. Windows refuses each
  write that reaches such a partition, even when its volume is locked and
  dismounted. A write past every partition went through.
- **Windows read the new table during the write.** With the old table zeroed,
  the first 4 MiB block put the Fedora table back, and the next write failed
  the same way.
- **Windows changed the stick after the write.** For a GPT that ends before
  the drive does, Windows moves the backup to the end of the drive and
  rewrites 12 bytes of the primary header. The check failed at byte 528. The
  ISO with those 12 bytes in it hashes to what the drive held, so the rest of
  the write was whole. The same 12 bytes also fail the media check of Fedora.
  Its MD5, computed the way `checkisomd5` computes it, matches the image as
  published and not the image with those 12 bytes.
- **An offline drive still changed after the check.** The check read the whole
  image back and matched it, and a minute later those 12 bytes were the ones
  of Windows again, with the drive offline the whole time.

The first three go when the first write takes the drive offline. An offline
drive has no partitions for Windows to apply. Offline, a write into the old
read-only partition went through, and the new header held after the write,
after the flush and after the handle closed. Online again, Windows rewrote it
within three seconds. A second write while the drive is offline passes too.

The fourth needs more: a read-only drive refuses the write that Windows makes.
The handle that wrote marks the drive read-only when it goes, which is after
the last write and before the check, and the check only reads. The next write
takes the mark off again. Measured: with the mark, the header held across the
close of the handle, a drive query and thirty seconds, and again across a
whole write, its check and the minute after it. The last line of a write says
that the drive is offline and read-only.

The first write also zeroes the MBR and both GPT copies, and the first block
of the image goes on last. So no stale backup stays at the end of the drive,
and a write that stops leaves no table.

Neither mark outlives the drive or the machine. Measured: the stick came out
and went back in, and Windows reported both flags gone and had changed the 12
bytes again within 20 seconds. A restart of Windows clears both marks as well,
measured with the stick still in the port. The media check of Fedora then
fails on that drive.

The first failure also showed a fault of the terminal: the error printed on the
end of the progress line. The line now ends before an error prints.

Each host above wrote the stick with the code as it stands, and each check
passed. **One claim is still open: the firmware of a physical PC.** A VM start
stands for it.

**Two faults that only a real run could find.**

- `fsync` on `/dev/rdiskN` answers `ENOTTY`, because the raw node holds no
  buffer of the operating system. The drive holds one of its own, so the flush
  is `DKIOCSYNCHRONIZECACHE`. Before the fix the bytes reached the medium and
  the command reported a failure.
- The Windows binary linked the Microsoft C runtime, so it stopped on a clean
  install before it ran one line. The build now links that runtime in.
- **Burnout offered to erase the live USB it was running from.** A live system
  reaches its root through a loop device: the medium holds one large file, the
  loop presents that file as a block device, and the root is an overlay on top
  of it. Nothing in that chain names the drive, so the drive carried no mark
  and no refusal. Found by booting a machine from a drive that Burnout had
  written. It now follows every mounted loop device to the drive that carries
  its backing file, and when it cannot name the system disk at all the
  confirmation asks for the model and the size rather than one word.

**Three measurements.** In a release build on an Apple M-series machine, a
2.7 GB write and its 2.7 GB read back take 22 seconds together, which is about
245 MB/s. The same work in a debug build takes 295 seconds, so measure the
build that ships. The hash is not the part that waits: a USB 3 stick writes at
30 to 150 MB/s.

---

## P3. The partition table and FAT32

**Size M. Depends on P2. Done.**

The first half of the Windows layout.

The work:

- Write an MBR: the boot signature, one entry per partition, the right type
  bytes, and CHS fields that are wrong in the way every tool writes them wrong.
- Format a FAT32 volume with the `fatfs` crate, which is MIT and works over
  anything that reads, writes and seeks.
- Put a sector adapter under it. `fatfs` writes a FAT32 entry as four bytes at
  `cluster * 4`, and a directory entry as 32 bytes, and a raw device refuses
  any write that its sector size does not divide. Measured in the 0.3.6
  source, and the `fatfs` README says to wrap the storage. The adapter reads a
  partial sector, changes it and writes it back, and passes whole sectors
  straight through. It lives in `burnout-core`, which keeps no dependency.
- Write a directory tree into it.
- Repair two faults in the directories that `fatfs` 0.3.6 writes. The next
  part of this section gives them.
- The layout lives in a new crate, `burnout-layout`, which is the only crate
  that depends on `fatfs`. P4 puts the exFAT writer beside it.

**The exit test.** A drive partitioned and filled by Burnout mounts on
Windows, on macOS and on Linux, and every file reads back with the hash it
went in with. The same code, run against an image file, passes in CI.

**It passes on the three hosts, and CI runs it on each push.**

The example `layout_image` writes the table, formats partition 1, copies a
sample tree of 207 files onto it and checks each file through a new mount.
The tree holds files at several depths, a file of no bytes, long names, the
name `Überprüfung.txt`, an empty directory, and a directory of 200 files that
fills more than one cluster. Each host then mounts the image with its own
mechanism, checks it with its own tool, and reads each file back through its
own FAT driver.

| Host | How it mounts the image | Its check tool | Files that match |
| --- | --- | --- | --- |
| macOS 26 | `hdiutil`, `mount -t msdos` | `fsck_msdos -n`, exit 0 | 207 of 207 |
| Fedora 44 ARM64, 512-byte sectors | `losetup` | `fsck.fat -n`, no fault | 207 of 207 |
| Fedora 44 ARM64, 4096-byte sectors | `losetup --sector-size 4096` | `fsck.fat -n`, no fault | 207 of 207 |
| Windows Server 2025, in CI | a VHD, `Mount-DiskImage -Access ReadOnly` | `chkdsk`, "found no problems" | 207 of 207 |

The `layout` job of CI runs the same checks on `ubuntu-latest`,
`macos-latest` and `windows-latest`, and its first run passed on each. Fedora
also ran them in a virtual machine here. The Windows machine here asks for an
administrator password before it mounts a VHD, and this work does not type
one, so the Windows answer is the runner of CI, which is an administrator.

The `same-image` job compares the image that each host built. The first run
got one digest from three hosts, `8da7a714...daadcf0ae`, and it is the digest
of the image built on macOS here. The same tree gives the same bytes on each
host, which is the promise of this project in its smallest form.

**Four faults that no test of this crate found, and the host tools did.**

- `fatfs` 0.3.6 writes a long-name entry before `.` and before `..` in each
  new directory, and gives `..` the cluster of the root, where FAT32 asks for
  0. `fsck_msdos -n` exits 206 and names every directory. The source of
  `fatfs` fixes both, and no release carries the fix: 0.3.6, of January 2023,
  is the last. A git or a patched dependency would stop `cargo install` from
  crates.io, so Burnout reads the volume after `fatfs` unmounts it and writes
  the two entries again. A test fails on the day a release of `fatfs` stops
  writing the old shape, and the repair can go then.
- `fatfs` without `chrono` writes a date of zero, which is not a date, and
  macOS shows each entry as 1970. Every entry now carries 1980-01-01 at
  midnight, the first second that FAT holds. One fixed time also keeps one
  tree one image.
- Linux could not find `Überprüfung.txt` until the mount had the `utf8`
  option. Without it, the kernel gives each name in its default character set.
  The volume was right, and macOS read the name.
- A test that changed one byte on the drive found the change in another file.
  The generator of the test bytes repeated a run of one file inside another,
  and the test searched for the run. The bytes now come from a generator
  that gives each file a stream of its own.

**What this does not prove.** Every write went to an image file. A raw device
is proven only through `StrictTarget`, which refuses what a device refuses.
P6 writes the layout to a device, and that is where the device half closes.

---

## P4. The exFAT writer

**Size XL. Depends on P3. Done. Ships as v0.2.**

The one piece with no crate behind it, and the reason macOS can take part at
all.

Microsoft published the exFAT specification in 2019, so this is documented
work and not archaeology.
That public specification is also the reason exFAT beat NTFS and APFS for
partition 2.

The work:

- The boot sector and its backup, with the checksum sector.
- The FAT.
- The allocation bitmap.
- The upcase table.
- Directory entries: the file entry, the stream extension, and the name
  entries, with the name hash and the checksum over the whole entry set.
- Large files, which is the entire point, so a file over 4 GiB is the first
  test and not the last.

**How the writer works.** These are the decisions, and the reason for each.

- **One pass, with the whole tree.** Burnout knows every file before it
  writes the first byte. So the writer plans the whole volume first, and then
  writes it. Each file and each directory gets one run of clusters, and its
  entry says so with the NoFatChain flag, as Windows does. The FAT holds a
  chain only for the allocation bitmap, the up-case table and the root
  directory, because the specification asks for a chain there.
- **The clusters go in the order of the tree.** The bitmap takes cluster 2,
  the up-case table comes next, and the root directory comes after them, as
  the specification recommends. Every directory and every file follows in the
  order of `TreePath`, so one tree gives one volume.
- **The cluster size is the one that Windows picks.** It is 4 KiB up to
  256 MiB, 32 KiB up to 32 GiB, and 128 KiB above that, and never less than
  one sector. The FAT starts 1 MiB into the volume, and the clusters start on
  the next whole mebibyte. That is the alignment of the partitions, for the
  same reason.
- **The boot regions go last.** The writer puts every other byte on the
  volume first. A drive that stops half way then holds nothing that a host
  mounts.
- **The up-case table is the one that the specification recommends.** It is
  2,918 entries in compressed form, and its checksum is E619D30Dh. The source
  holds it as a table whose lines match Table 25 of the specification, and a
  test checks the checksum.
- **Every timestamp is 1980-01-01 at midnight, in local time,** as on
  partition 1. One tree then gives one volume, to the byte.
- **Each file and each directory fills whole clusters, and the rest of its
  last cluster is zero.** Old data on a drive then cannot show through, and
  the same tree gives the same bytes on a device too.
- **The check reads the volume with a reader of its own.** It shares only
  the checksums and the up-case table with the writer, and no cache. It
  checks the boot region, its checksum and
  its copy, the checksum of each entry set and the hash of each name. It
  checks that the bitmap marks each cluster that the volume uses and no other.
  Then it reads each file and compares its digest with the one it went in
  with. The host tools check the same volume in CI.
- **The code lives in `burnout-layout`, beside FAT32.** It takes no
  dependency.

**The exit test.** A volume that Burnout formats mounts on Windows, on macOS
and on Linux. A 6 GiB file written into it reads back with the same hash on
all three. The host's own check tool reports no error.

**It passes on the three hosts, and CI runs it on each push.**

The example `layout_image` now also writes partition 2 as exFAT. It holds the
same tree of 207 files and one more: `sources/install.wim` of 6,442,463,289
bytes, which is 6 GiB and 12,345 bytes. Its bytes come from a generator, so no
host holds a file of that size. The example checks both partitions with its
own readers. Each host then mounts the image with its own mechanism, checks
each partition with its own tool, and reads each file back through its own
driver.

| Host | How it mounts the image | Its exFAT check tool | Files of partition 2 that match |
| --- | --- | --- | --- |
| macOS 26 | `hdiutil`, `mount -t exfat` | `fsck_exfat -n`, "appears to be OK" | 208 of 208 |
| Fedora 44 ARM64, 512-byte sectors | `losetup` | `fsck.exfat -n` of exfatprogs 1.3.2, "clean" | 208 of 208 |
| Fedora 44 ARM64, 4096-byte sectors | `losetup --sector-size 4096` | the same | 208 of 208 |
| Ubuntu 24.04 in CI, at both sizes | `losetup` | `fsck.exfat -n` of exfatprogs 1.2.2, "clean" | 208 of 208 |
| Windows Server 2025, in CI | a VHD, `Mount-DiskImage -Access ReadOnly` | `chkdsk`, "found no problems" | 208 of 208 |

Partition 1 passes on each host too, with 207 of 207 files, as in P3. The
large file has the SHA-256 `9f122a80...b7b8fa6d20` on each host, which is the
digest it went in with. The `same-image` job got one digest of the whole
image from the three hosts, `0b3d05d3...c536408dc`, and it is the digest of
the image built on macOS here.

**What the host tools found.** Nothing in the volume. Each check tool passed
the first volume that the writer made. Two things around it:

- The Linux runner of CI, Ubuntu 24.04 with the kernel 6.17.0-1022-azure,
  had no exFAT driver. `fsck.exfat` passed the volume, and `mount -t exfat`
  said that it does not know the file system. The driver is in
  `linux-modules-extra` of that kernel, and CI now installs it when
  `modprobe exfat` finds no module.
- macOS shows each time on both partitions as 23:00 on 31 December 1979, in
  the zone of the Mac here, which is one hour ahead of UTC in January. The
  entries hold midnight on 1 January 1980. The two partitions agree, because
  the exFAT entries hold no offset from UTC, which the specification allows,
  and which means local time, as FAT keeps it.

**The checksums are the ones that macOS writes.** The tests hold entry sets,
entries of the root and a boot region from a volume that macOS 26 formatted.
The writer gives the same bytes and the same checksums. The reader also reads
that volume of macOS whole, bitmap and all.

**What this does not prove.** Every write went to an image file. A raw device
is proven only through `StrictTarget`, which refuses what a device refuses.
P6 writes the layout to a device, and that is where the device half closes.
The Windows machine here asks for an administrator password before it mounts
a VHD, so the Windows answer is the runner of CI, as in P3.

---

## P5. Reading an ISO

**Size L. Depends on nothing. Start it beside P3.**

Burnout has to read the image itself, for the same reason it writes the
filesystem itself: a mount through the host is three different behaviours.

**This phase is larger than the milestones document assumed.**
A Windows ISO is UDF, not ISO 9660.
ISO 9660 stores an extent length in 32 bits, so it cannot hold a file of 4 GiB
or more, and a current `install.wim` is exactly that.
[The spike](spike-layout.md) measured both.
So reading a Windows ISO needs a UDF reader, and Burnout writes one.

The work:

- ISO 9660, with Joliet and Rock Ridge, for a Linux ISO.
- **UDF, read only**: the anchor descriptor, the logical volume and partition
  descriptors, the file set, file entries, directory descriptors, and plain
  extents. No compression, no encryption, no write path. This is decided, and
  the reason is below.
- Detect the mode from the first 512 bytes and from what is inside.
- Refuse a macOS installer by name, and say `createinstallmedia`.

**The exit test.** Burnout lists the contents of a Windows 11 ISO and a Ubuntu
ISO, and extracts `install.wim` from the first with the hash that the host's
own mount gives for the same file.

---

## P6. Windows mode

**Size L. Depends on P0, P4 and P5. Ships as v0.3.**

Put the four pieces together.

The work:

- Plan the layout, then write it: MBR, FAT32 of about 1 GB, exFAT for the rest.
- Copy every boot file to partition 1 and the install image to partition 2.
- Generate `autounattend.xml`: the `LabConfig` keys for the hardware checks,
  a local account for the Microsoft account step, and the `InstallFrom` path
  if P0 says it is needed. Write only the entries that the person asks for,
  and leave the rest of Setup interactive.
- Per-file verification, and a report that names the check it ran. A hash of
  the whole device proves nothing here, because Burnout built a filesystem
  that the source never had.

**The exit test.** A drive made on each of the three hosts installs Windows 11
on a machine with no TPM, with no Microsoft account step, from an ISO whose
`install.wim` is over 4 GiB.

---

## P7. The edges, and version 1

**Size M. Depends on P6. Ships as v1.0.**

The work that turns a program that works into one somebody else can use.

- One error message for every refusal, in Simplified Technical English, that
  names the fix.
- `--json` for the list and for progress, so a script can read it.
- Resume nothing and promise nothing about a stopped write. Say that the drive
  is now unusable, because it is.
- Build provenance is in place from v0.1, because it moved to P2. This phase
  checks that it still holds for every artifact and every host.
- The macOS and Windows download warning, and the way past it, in the README.
- `install.esd` alongside `install.wim` everywhere.
- The README stops saying that nothing is built.

**The exit test.** Somebody who has never seen the project writes a Windows
drive and a Linux drive, on a host you did not choose, without asking you a
question.

---

## The decisions that were open

All three are settled. The reasons are here so that a later reader can reopen
one with an argument rather than a preference.

### A fixed disk can be a target, and it costs two steps

Every drive in a desktop computer reports as fixed, so a refusal by that test
alone stops somebody writing an internal disk on purpose.
An easy yes stops nobody writing the wrong one by accident.

So `--force` allows a fixed disk, **and the person types the model and the
size of the drive back before the write starts**.
Not a letter, and not the word yes.
A prompt that takes one keystroke is a prompt that people learn to answer
without reading.

**The system disk is refused under every flag.** There is no escape for it,
and that is what [SECURITY.md](../SECURITY.md) already promises.

### Burnout reads UDF itself

A Windows ISO keeps `install.wim` in UDF, because ISO 9660 holds an extent
length in 32 bits and cannot address a file of 4 GiB or more.
[The spike](spike-layout.md) measured both facts.

Burnout gets a read-only UDF reader, scoped to what a Windows ISO uses: the
anchor descriptor, the logical volume and partition descriptors, the file set,
file entries, directory descriptors, and plain extents.
No compression, no encryption, and no write path.

Three reasons. It keeps the rule that Burnout never delegates to the host, so
one ISO reads the same way on three operating systems. ECMA-167 and the UDF
specification are public, so this is documented work. And it runs against a
file, so every test of it obeys the rule that no test opens a device.

Read the licence and the coverage of any crate before counting on it, but do
not plan around one.

**The fallback stays on the shelf:** take an extracted folder instead of an
ISO. It is honest and nearly free, and it makes the tool worse at the one
thing it exists to do.

### Version 1 buys no certificate

A code signing certificate matters for a binary that somebody **downloads**,
not one that a package manager installs.

- macOS applies Gatekeeper to a file that carries `com.apple.quarantine`,
  which a browser sets. Homebrew fetches with curl and does not set it.
  `cargo install` compiles on the machine, so there is nothing to quarantine.
- Windows SmartScreen keys off the Mark of the Web, which a browser sets.
- Linux never expected it. A distribution signs with its own key.

Apple silicon does need every binary to carry a signature, and an ad hoc one
counts. The Rust linker applies that already.

So the path that a certificate would help is one person downloading a built
binary from a release page in a browser. That is real, and it is not worth
several hundred a year for a project with no users yet.

**Instead, every release carries build provenance.**
`actions/attest-build-provenance` signs each artifact through Sigstore, and
anybody can check it with `gh attestation verify`.
A certificate proves that somebody paid a fee.
An attestation proves that this binary came from this commit through this
workflow, which is the stronger claim for a tool that asks for root.
Publish a SHA256 for each artifact as well, and document the warning and the
way past it for anybody who downloads one directly.

---

## What version 1 is not

Legacy BIOS boot, Windows To Go, a drive that holds many images, a write to a
single partition, a graphical interface, and macOS install media.
[The milestones](milestones.md) say why for each one.
