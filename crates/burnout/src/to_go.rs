//! A drive of Windows To Go: one edition of the install image of a Windows
//! ISO, applied to an NTFS partition, with the boot files and a BCD store on
//! an EFI system partition. The drive starts Windows itself through UEFI.
//!
//! The layout is a GPT with two partitions. The EFI system partition goes
//! from 1 MiB for 300 MiB, in FAT32. The Windows partition goes from 512
//! MiB to 1 MiB before the end of the drive, in NTFS. The last MiB holds the
//! copy of the GPT. A drive with this layout, written to a disk image,
//! started Windows 11 25H2 Pro of the Arm64 ISO to its first-run setup in
//! QEMU on 2026-11-11, with no program of Windows in the path.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};

use burnout_core::{BlockTarget, Error, MemorySource, Result, SectorIo, Window};
use burnout_layout::bcd::{store_bytes, BcdDrive};
use burnout_layout::gpt::{gpt, GptPartition, BASIC_DATA, EFI_SYSTEM};
use burnout_layout::ntfs::{format, Formatted, NewFile, NtfsOptions, Run, Times, CLUSTER};
use burnout_layout::{copy_to_fat32, format_fat32, verify_fat32, Fat32Options, Manifest};
use burnout_wim::sha1::{Digest, Sha1};
use burnout_wim::{copy_resource, parse_metadata, read_blobs, read_resource, Arch, Blob, Wim};

/// The first sector of the EFI system partition, and its length.
pub const EFI_FIRST_SECTOR: u64 = 2048;
pub const EFI_SECTORS: u64 = 300 * 2048;
/// The first sector of the Windows partition.
pub const WINDOWS_FIRST_SECTOR: u64 = 512 * 2048;
/// The bytes at the end of the drive that no partition takes. The copy of
/// the GPT goes there.
const TAIL_BYTES: u64 = 1024 * 1024;
/// The sector size of the layout. A drive with other sectors is refused.
const SECTOR: u32 = 512;
/// The time on the new volumes: 2025-11-11, in the units of NTFS and of
/// the registry.
const TIME: u64 = 134_070_000_000_000_000;

/// The attributes of Windows that a file keeps on the new volume. Sparse,
/// compressed and encrypted need attributes of their own, which the writer
/// does not make yet.
const KEPT: u32 = 0x0001 | 0x0002 | 0x0004 | 0x0010 | 0x0020 | 0x0080 | 0x0100 | 0x1000 | 0x2000;

/// One stream of the edition and where it goes.
#[derive(Clone, Debug)]
struct Fill {
    /// The offset of its resource in the WIM file, so that the streams go
    /// in the order of the file.
    offset: u64,
    hash: Digest,
    /// The file in the list, and the named stream of the file, or the data
    /// of the file when there is none.
    file: usize,
    stream: Option<usize>,
}

/// Everything that goes onto the drive, worked out before the first write.
#[derive(Debug)]
pub struct ToGo {
    /// The name of the edition, as a person reads it.
    pub edition: String,
    /// Files and directories, and the names after the first of each file.
    pub files: usize,
    pub links: usize,
    /// The bytes of the streams that go onto the Windows partition.
    pub stream_bytes: u64,
    /// The boot files on the EFI system partition, without the BCD store.
    pub boot_files: usize,
    drive_bytes: u64,
    windows_bytes: u64,
    fills: Vec<Fill>,
    sizes: HashMap<Digest, u64>,
    ntfs: Formatted,
    boot: MemorySource,
    disk_id: [u8; 16],
    efi_id: [u8; 16],
    windows_id: [u8; 16],
}

/// Written to the drive: the files of the EFI system partition, for the
/// check.
#[derive(Debug)]
pub struct Written {
    efi: Manifest,
}

fn image_error(name: &str, detail: impl std::fmt::Display) -> Error {
    Error::Image {
        path: name.to_string(),
        detail: detail.to_string(),
    }
}

/// 16 bytes for a GUID, from a seed and a number. The GUIDs of one drive
/// differ from those of every other drive when the seeds differ.
fn new_guid(seed: &str, n: u8) -> [u8; 16] {
    let d = burnout_core::sha256(format!("{seed}/{n}").as_bytes());
    let mut g: [u8; 16] = d.as_bytes()[..16].try_into().expect("16 bytes");
    // Version 4 and the variant of RFC 4122, as a random GUID has them.
    g[7] = (g[7] & 0x0F) | 0x40;
    g[8] = (g[8] & 0x3F) | 0x80;
    g
}

/// The smallest drive that holds `edition_bytes` of files, with room for
/// the metadata of NTFS. An estimate: the layout itself is the proof.
pub fn needed_bytes(edition_bytes: u64) -> u64 {
    WINDOWS_FIRST_SECTOR * u64::from(SECTOR) + edition_bytes + edition_bytes / 10 + TAIL_BYTES
}

/// Work out the drive for edition `edition`, the position of the edition in
/// the install image counted from 1. `seed` makes the GUIDs of the drive.
pub fn plan<R: Read + Seek>(
    wim_file: &mut R,
    name: &str,
    wim: &Wim,
    edition: usize,
    drive_bytes: u64,
    seed: &str,
) -> Result<ToGo> {
    let info = wim
        .editions
        .get(edition.wrapping_sub(1))
        .ok_or_else(|| image_error(name, format!("the install image has no edition {edition}")))?;
    if let Some(total) = info.total_bytes {
        if drive_bytes < needed_bytes(total) {
            return Err(Error::NeedsLargerDrive {
                needed_bytes: needed_bytes(total),
                drive_bytes,
            });
        }
    }
    let blobs = read_blobs(&mut *wim_file, &wim.header, name)?;
    let by_hash: HashMap<Digest, &Blob> = blobs.iter().map(|b| (b.hash, b)).collect();
    let mut metas: Vec<&Blob> = blobs.iter().filter(|b| b.is_metadata()).collect();
    metas.sort_by_key(|b| b.resource.offset);
    let meta = metas.get(edition - 1).ok_or_else(|| {
        image_error(
            name,
            format!("the install image has no tree for edition {edition}"),
        )
    })?;
    let bytes = read_resource(&mut *wim_file, &wim.header, &meta.resource, name)?;
    let tree = parse_metadata(&bytes).map_err(|e| image_error(name, e))?;

    // The new files, in the order of the tree. A hard link becomes a
    // second name of the file that it links to.
    let mut list: Vec<NewFile> = Vec::new();
    let mut index_of: HashMap<&str, usize> = HashMap::new();
    let mut by_group: HashMap<u64, usize> = HashMap::new();
    let mut fills: Vec<Fill> = Vec::new();
    let size_of = |h: &Digest| by_hash.get(h).map(|b| b.resource.original_bytes);
    for d in tree.entries.iter().skip(1) {
        let (parent_path, leaf) = d.path.rsplit_once('/').unwrap_or(("", &d.path));
        let parent = if parent_path.is_empty() {
            None
        } else {
            Some(
                *index_of
                    .get(parent_path)
                    .ok_or_else(|| image_error(name, format!("{} has no parent", d.path)))?,
            )
        };
        if d.hard_link_group != 0 && !d.is_directory() {
            if let Some(&first) = by_group.get(&d.hard_link_group) {
                list[first].links.push((parent, leaf.to_string()));
                continue;
            }
        }
        let mut f = NewFile {
            parent,
            name: leaf.to_string(),
            directory: d.is_directory(),
            size: 0,
            attributes: d.attributes & KEPT,
            times: Times {
                creation: d.creation_time,
                modification: d.last_write_time,
                record_change: d.last_write_time,
                access: d.last_access_time,
            },
            security: d.security_id.map(|s| s as usize),
            links: Vec::new(),
            reparse: None,
            streams: Vec::new(),
        };
        let data = d.data_hash().copied();
        let missing = || {
            image_error(
                name,
                format!("{} names a stream that the image does not hold", d.path),
            )
        };
        if let Some(tag) = d.reparse_tag {
            let body = match &data {
                Some(h) => {
                    let b = by_hash.get(h).ok_or_else(missing)?;
                    read_resource(&mut *wim_file, &wim.header, &b.resource, name)?
                }
                None => Vec::new(),
            };
            let mut rp = tag.to_le_bytes().to_vec();
            rp.extend_from_slice(&(body.len() as u16).to_le_bytes());
            rp.extend_from_slice(&[0, 0]);
            rp.extend_from_slice(&body);
            f.reparse = Some(rp);
        } else if let Some(h) = data {
            f.size = size_of(&h).ok_or_else(missing)?;
        }
        let i = list.len();
        if let (Some(h), None, false) = (data, d.reparse_tag, d.is_directory()) {
            if f.size > 0 {
                fills.push(Fill {
                    offset: by_hash[&h].resource.offset,
                    hash: h,
                    file: i,
                    stream: None,
                });
            }
        }
        for s in d.streams.iter().filter(|s| !s.name.is_empty()) {
            let size = if s.hash == [0; 20] {
                0
            } else {
                size_of(&s.hash).ok_or_else(missing)?
            };
            if size > 0 {
                fills.push(Fill {
                    offset: by_hash[&s.hash].resource.offset,
                    hash: s.hash,
                    file: i,
                    stream: Some(f.streams.len()),
                });
            }
            f.streams.push((s.name.clone(), size));
        }
        if f.directory {
            index_of.insert(&d.path, i);
        }
        if d.hard_link_group != 0 && !f.directory {
            by_group.insert(d.hard_link_group, i);
        }
        list.push(f);
    }
    fills.sort_by_key(|f| f.offset);

    let first = WINDOWS_FIRST_SECTOR * u64::from(SECTOR);
    if drive_bytes < first + TAIL_BYTES + 64 * 1024 * 1024 {
        return Err(Error::NeedsLargerDrive {
            needed_bytes: needed_bytes(0),
            drive_bytes,
        });
    }
    // Whole MiB, so the partition ends where it can on any drive.
    let windows_bytes = (drive_bytes - first - TAIL_BYTES) / (1024 * 1024) * (1024 * 1024);
    let seed_serial = new_guid(seed, 6);
    let ntfs = format(
        windows_bytes,
        WINDOWS_FIRST_SECTOR as u32,
        &NtfsOptions {
            label: "WINDOWS".into(),
            serial: u64::from_le_bytes(seed_serial[..8].try_into().expect("8 bytes")),
            time: TIME,
        },
        &list,
        &tree.security,
    )
    .map_err(|e| image_error(name, format!("the edition does not fit on NTFS: {e}")))?;

    // The boot files: Windows\Boot\EFI and its fonts, as bcdboot copies
    // them, and the boot manager again as the file that the firmware starts.
    let firmware = if info.arch == Some(Arch::Arm64) {
        "EFI/Boot/BOOTAA64.EFI"
    } else {
        "EFI/Boot/BOOTX64.EFI"
    };
    let mut boot = MemorySource::new();
    let mut boot_files = 0;
    let mut manager = false;
    for d in &tree.entries {
        let lower = d.path.to_ascii_lowercase();
        let dest = if let Some(rest) = lower.strip_prefix("windows/boot/efi/") {
            format!(
                "EFI/Microsoft/Boot/{}",
                &d.path[d.path.len() - rest.len()..]
            )
        } else if let Some(rest) = lower.strip_prefix("windows/boot/fonts/") {
            format!(
                "EFI/Microsoft/Boot/Fonts/{}",
                &d.path[d.path.len() - rest.len()..]
            )
        } else {
            continue;
        };
        if d.is_directory() {
            boot.add_dir(&dest)?;
            continue;
        }
        let bytes = match d.data_hash() {
            Some(h) => {
                let b = by_hash
                    .get(h)
                    .ok_or_else(|| image_error(name, format!("{} names no stream", d.path)))?;
                read_resource(&mut *wim_file, &wim.header, &b.resource, name)?
            }
            None => Vec::new(),
        };
        if lower == "windows/boot/efi/bootmgfw.efi" {
            boot.add_file(firmware, bytes.clone())?;
            manager = true;
        }
        boot.add_file(&dest, bytes)?;
        boot_files += 1;
    }
    if !manager {
        return Err(image_error(
            name,
            format!(
                "edition {edition} has no Windows\\Boot\\EFI\\bootmgfw.efi, so a drive of it cannot start"
            ),
        ));
    }
    let (disk_id, efi_id, windows_id) = (new_guid(seed, 1), new_guid(seed, 2), new_guid(seed, 3));
    let store = store_bytes(
        &BcdDrive {
            disk: disk_id,
            efi_partition: efi_id,
            windows_partition: windows_id,
            loader: new_guid(seed, 4),
            resume: new_guid(seed, 5),
            description: info.shown_name().to_string(),
            locale: if info.default_language.is_empty() {
                "en-US".to_string()
            } else {
                info.default_language.clone()
            },
        },
        TIME,
    )
    .map_err(|e| image_error(name, format!("the BCD store: {e}")))?;
    boot.add_file("EFI/Microsoft/Boot/BCD", store)?;
    // The GPT goes into the drive plan as a check here, and onto the drive
    // in `write`.
    gpt_for(drive_bytes, disk_id, efi_id, windows_id, windows_bytes)
        .map_err(|e| image_error(name, e))?;

    let sizes = fills
        .iter()
        .map(|f| (f.hash, by_hash[&f.hash].resource.original_bytes))
        .collect::<HashMap<_, _>>();
    let stream_bytes = fills.iter().map(|f| sizes[&f.hash]).sum();
    let links = list.iter().map(|f| f.links.len()).sum();
    Ok(ToGo {
        edition: info.shown_name().to_string(),
        files: list.len(),
        links,
        stream_bytes,
        boot_files,
        drive_bytes,
        windows_bytes,
        fills,
        sizes,
        ntfs,
        boot,
        disk_id,
        efi_id,
        windows_id,
    })
}

fn gpt_for(
    drive_bytes: u64,
    disk: [u8; 16],
    efi: [u8; 16],
    windows: [u8; 16],
    windows_bytes: u64,
) -> std::result::Result<burnout_layout::gpt::Gpt, String> {
    gpt(
        drive_bytes / u64::from(SECTOR),
        disk,
        &[
            GptPartition {
                kind: EFI_SYSTEM,
                id: efi,
                first: EFI_FIRST_SECTOR,
                last: EFI_FIRST_SECTOR + EFI_SECTORS - 1,
                name: "EFI system partition".to_string(),
            },
            GptPartition {
                kind: BASIC_DATA,
                id: windows,
                first: WINDOWS_FIRST_SECTOR,
                last: WINDOWS_FIRST_SECTOR + windows_bytes / u64::from(SECTOR) - 1,
                name: "Windows".to_string(),
            },
        ],
    )
}

impl ToGo {
    /// The bytes that `write` puts on the drive and `verify` reads back,
    /// for a bar of progress.
    pub fn total_bytes(&self) -> u64 {
        self.stream_bytes
            + self
                .ntfs
                .writes
                .iter()
                .map(|(_, b)| b.len() as u64)
                .sum::<u64>()
    }
}

/// Write the drive: the GPT, the EFI system partition with its files, and
/// the NTFS partition with every stream of the edition. `tally` hears the
/// bytes of each piece as it goes out.
pub fn write<T: BlockTarget, R: Read + Seek>(
    drive: &mut T,
    plan: &ToGo,
    wim_file: &mut R,
    wim: &Wim,
    name: &str,
    tally: &mut dyn FnMut(u64),
) -> Result<Written> {
    if drive.logical_sector_size() != SECTOR {
        return Err(Error::SectorSize {
            got: drive.logical_sector_size(),
        });
    }
    if drive.length() != plan.drive_bytes {
        return Err(Error::DriveChanged {
            wanted: format!("a drive of {} bytes", plan.drive_bytes),
            found: format!("a drive of {} bytes", drive.length()),
        });
    }
    let table = gpt_for(
        plan.drive_bytes,
        plan.disk_id,
        plan.efi_id,
        plan.windows_id,
        plan.windows_bytes,
    )
    .map_err(|e| image_error(name, e))?;
    {
        let mut io = SectorIo::new(&mut *drive);
        io.seek(SeekFrom::Start(0))?;
        io.write_all(&table.head)?;
        io.seek(SeekFrom::Start(table.tail_sector * u64::from(SECTOR)))?;
        io.write_all(&table.tail)?;
        io.flush()?;
    }

    let efi = efi_window(drive)?;
    format_fat32(
        efi,
        &Fat32Options {
            label: "SYSTEM",
            serial: u32::from_le_bytes(plan.disk_id[..4].try_into().expect("4 bytes")),
            first_sector: EFI_FIRST_SECTOR as u32,
        },
    )?;
    let manifest = copy_to_fat32(efi_window(drive)?, &plan.boot)?;

    let base = WINDOWS_FIRST_SECTOR * u64::from(SECTOR);
    let mut io = SectorIo::new(Window::new(&mut *drive, base, plan.windows_bytes)?);
    for (at, bytes) in &plan.ntfs.writes {
        io.seek(SeekFrom::Start(*at))?;
        io.write_all(bytes)?;
        tally(bytes.len() as u64);
    }
    let by_hash: HashMap<Digest, Blob> = read_blobs(&mut *wim_file, &wim.header, name)?
        .into_iter()
        .map(|b| (b.hash, b))
        .collect();
    for fill in &plan.fills {
        let run = run_of(plan, fill)
            .ok_or_else(|| image_error(name, "a stream with bytes has no clusters"))?;
        io.seek(SeekFrom::Start(run.lcn.expect("a placed run") * CLUSTER))?;
        let blob = &by_hash[&fill.hash];
        copy_resource(&mut *wim_file, &wim.header, &blob.resource, name, &mut io)?;
        tally(blob.resource.original_bytes);
    }
    io.flush()?;
    drop(io);
    drive.sync()?;
    Ok(Written { efi: manifest })
}

fn efi_window<T: BlockTarget>(drive: &mut T) -> Result<Window<&mut T>> {
    Window::new(
        drive,
        EFI_FIRST_SECTOR * u64::from(SECTOR),
        EFI_SECTORS * u64::from(SECTOR),
    )
}

fn run_of(plan: &ToGo, fill: &Fill) -> Option<Run> {
    match fill.stream {
        None => plan.ntfs.placements[fill.file],
        Some(s) => plan.ntfs.stream_placements[fill.file][s],
    }
}

/// Read the drive back: each file of the EFI system partition through a new
/// mount, each block of NTFS metadata, and each stream of the edition
/// against the SHA-1 that the install image names for it.
pub fn verify<T: BlockTarget>(
    drive: &mut T,
    plan: &ToGo,
    written: &Written,
    tally: &mut dyn FnMut(u64),
) -> Result<()> {
    verify_fat32(efi_window(drive)?, &written.efi)?;
    let base = WINDOWS_FIRST_SECTOR * u64::from(SECTOR);
    let mut io = SectorIo::new(Window::new(&mut *drive, base, plan.windows_bytes)?);
    for (at, bytes) in &plan.ntfs.writes {
        let mut back = vec![0u8; bytes.len()];
        io.seek(SeekFrom::Start(*at))?;
        io.read_exact(&mut back)?;
        if &back != bytes {
            return Err(Error::VolumeDiffers {
                path: "the NTFS metadata".to_string(),
                detail: format!(
                    "the {} bytes at byte {at} of the partition differ",
                    bytes.len()
                ),
            });
        }
        tally(bytes.len() as u64);
    }
    let mut buffer = vec![0u8; 1024 * 1024];
    for fill in &plan.fills {
        let run = run_of(plan, fill).expect("write placed it");
        io.seek(SeekFrom::Start(run.lcn.expect("a placed run") * CLUSTER))?;
        let mut left = plan.sizes[&fill.hash];
        let mut sha = Sha1::new();
        while left > 0 {
            let n = left.min(buffer.len() as u64) as usize;
            io.read_exact(&mut buffer[..n])?;
            sha.update(&buffer[..n]);
            left -= n as u64;
            tally(n as u64);
        }
        if sha.finish() != fill.hash {
            return Err(Error::VolumeDiffers {
                path: format!(
                    "the stream at cluster {} of the NTFS partition",
                    run.lcn.unwrap_or(0)
                ),
                detail: format!(
                    "its SHA-1 is not {}, which the install image names",
                    burnout_wim::sha1::hex(&fill.hash)
                ),
            });
        }
    }
    drop(io);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burnout_core::{read_table, FileTarget, PartitionType, TableKind};
    use std::fs::File;
    use std::path::PathBuf;

    const MIB: u64 = 1024 * 1024;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../burnout-wim/tests/data/{name}"))
    }

    /// A sparse file of `bytes` as the drive, which the test removes.
    struct Drive(PathBuf);

    impl Drive {
        fn new(test: &str, bytes: u64) -> (Drive, FileTarget) {
            let path = std::env::temp_dir()
                .join(format!("burnout-to-go-{test}-{}.img", std::process::id()));
            let target = FileTarget::create(&path, bytes, 512).unwrap();
            (Drive(path), target)
        }
    }

    impl Drop for Drive {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn open(name: &str) -> (File, Wim) {
        let path = fixture(name);
        let wim = burnout_wim::read_wim(File::open(&path).unwrap(), name).unwrap();
        (File::open(&path).unwrap(), wim)
    }

    #[test]
    fn a_drive_reads_back_with_its_table_its_boot_files_and_each_stream() {
        let (mut file, wim) = open("to-go.wim");
        let bytes = 600 * MIB;
        let plan = plan(&mut file, "to-go.wim", &wim, 1, bytes, "test").unwrap();
        // Windows, Boot, EFI, en-US, Fonts, System32, drivers, and the five
        // files. The second name of notepad.exe is a link.
        assert_eq!((plan.files, plan.links), (12, 1));
        assert_eq!(plan.boot_files, 3);
        let (_keep, mut drive) = Drive::new("reads-back", bytes);
        let written = write(&mut drive, &plan, &mut file, &wim, "to-go.wim", &mut |_| {}).unwrap();
        verify(&mut drive, &plan, &written, &mut |_| {}).unwrap();

        let mut head = vec![0u8; MIB as usize];
        drive.seek(SeekFrom::Start(0)).unwrap();
        drive.read_exact(&mut head).unwrap();
        let table = read_table(&head, 512, bytes).unwrap().unwrap();
        assert_eq!(table.kind, TableKind::Gpt);
        assert_eq!(table.partitions.len(), 2);
        assert_eq!(table.partitions[0].first_byte, MIB);
        assert_eq!(table.partitions[0].bytes, 300 * MIB);
        assert!(table.partitions[0]
            .kind
            .describe()
            .starts_with("EFI system"));
        assert_eq!(table.partitions[1].first_byte, 512 * MIB);
        assert_eq!(table.partitions[1].bytes, 87 * MIB);
        let PartitionType::Gpt { name, .. } = &table.partitions[1].kind else {
            panic!("a GPT partition")
        };
        assert_eq!(name, "Windows");

        // The NTFS boot sector of the Windows partition.
        let mut boot = vec![0u8; 512];
        drive.seek(SeekFrom::Start(512 * MIB)).unwrap();
        drive.read_exact(&mut boot).unwrap();
        assert_eq!(&boot[3..11], b"NTFS    ");
    }

    #[test]
    fn a_changed_byte_of_a_stream_fails_the_check() {
        let (mut file, wim) = open("to-go.wim");
        let bytes = 600 * MIB;
        let plan = plan(&mut file, "to-go.wim", &wim, 1, bytes, "test").unwrap();
        let (_keep, mut drive) = Drive::new("changed", bytes);
        let written = write(&mut drive, &plan, &mut file, &wim, "to-go.wim", &mut |_| {}).unwrap();
        let last = plan.fills.last().unwrap();
        let run = run_of(&plan, last).unwrap();
        let at = 512 * MIB + run.lcn.unwrap() * CLUSTER;
        let mut sector = vec![0u8; 512];
        drive.seek(SeekFrom::Start(at)).unwrap();
        drive.read_exact(&mut sector).unwrap();
        sector[100] ^= 0xFF;
        drive.seek(SeekFrom::Start(at)).unwrap();
        drive.write_all(&sector).unwrap();
        let e = verify(&mut drive, &plan, &written, &mut |_| {}).unwrap_err();
        assert!(matches!(e, Error::VolumeDiffers { .. }), "{e}");
        assert!(e.to_string().contains("SHA-1"), "{e}");
    }

    #[test]
    fn an_edition_without_a_boot_manager_is_refused() {
        let (mut file, wim) = open("lzx-32k.wim");
        let e = plan(&mut file, "lzx-32k.wim", &wim, 1, 600 * MIB, "test").unwrap_err();
        assert!(e.to_string().contains("bootmgfw.efi"), "{e}");
    }

    #[test]
    fn a_drive_without_room_for_the_layout_is_refused() {
        let (mut file, wim) = open("to-go.wim");
        let e = plan(&mut file, "to-go.wim", &wim, 1, 520 * MIB, "test").unwrap_err();
        assert!(matches!(e, Error::NeedsLargerDrive { .. }), "{e}");
    }

    #[test]
    fn two_seeds_give_two_sets_of_guids() {
        assert_ne!(new_guid("a", 1), new_guid("b", 1));
        assert_ne!(new_guid("a", 1), new_guid("a", 2));
        let g = new_guid("a", 1);
        assert_eq!(g[7] >> 4, 4);
        assert_eq!(g[8] >> 6, 2);
    }
}
