//! An empty NTFS volume: the boot sector, the master file table with the
//! system files, and the data of each system file.
//!
//! The system files and their attributes follow the volume that format of
//! Windows 11 25H2 wrote on 2026-11-11: records 0 to 15, records 16 to 23
//! free, and $Quota, $ObjId and $Reparse in $Extend as records 24 to 26.
//! Windows adds its own files to $Extend when it mounts the volume.

use crate::exfat::upcase::UpCase;
use crate::ntfs::attrdef::{attrdef, ATTRDEF_BYTES};
use crate::ntfs::boot::{boot_sector, Geometry};
use crate::ntfs::descriptor::{system_descriptor, READ, READ_WRITE};
use crate::ntfs::index::{build, Entry, Index, Shape};
use crate::ntfs::record::{
    kind, reference, Attribute, Record, Value, FLAG_DIRECTORY, FLAG_IN_USE, FLAG_VIEW_INDEX,
    RECORD_BYTES,
};
use crate::ntfs::runs::Run;
use crate::ntfs::secure::{secure, FIRST_ID};
use crate::ntfs::values::{file_attribute as fa, standard_information, FileName, Namespace, Times};

pub(crate) const CLUSTER: u64 = 4096;
const SECTOR: u64 = 512;
const INDEX_BLOCK: usize = 4096;
/// The records that the master file table starts with.
const MFT_RECORDS: u64 = 256;
const ROOT: u64 = 5;
const EXTEND: u64 = 11;
/// The first record of the files in $Extend.
const QUOTA: u64 = 24;
const OBJ_ID: u64 = 25;
const REPARSE: u64 = 26;
/// The first record that a file of the person can take.
pub(crate) const FIRST_FREE_RECORD: u64 = 27;
/// The security IDs of the system files: read, and read and write.
const SD_READ: u32 = FIRST_ID;
const SD_READ_WRITE: u32 = FIRST_ID + 1;
/// The collation rules of the indexes.
const COLLATE_FILE_NAME: u32 = 0x01;
const COLLATE_ULONG: u32 = 0x10;
const COLLATE_SID: u32 = 0x11;
const COLLATE_SECURITY_HASH: u32 = 0x12;
const COLLATE_ULONGS: u32 = 0x13;
/// The flag of a record of a file in $Extend, as Windows sets it.
const FLAG_EXTEND_FILE: u16 = 0x0004;

/// What the person chooses for a new volume.
#[derive(Clone, Debug)]
pub(crate) struct NtfsOptions {
    pub label: String,
    pub serial: u64,
    /// The time of each file, in 100 ns units since 1601.
    pub time: u64,
}

/// A new volume: the bytes to write, each at its byte of the volume.
#[derive(Clone, Debug)]
pub(crate) struct Formatted {
    pub geometry: Geometry,
    pub writes: Vec<(u64, Vec<u8>)>,
    /// The clusters of the volume, and the first one that no file holds.
    pub clusters: u64,
    pub first_free_cluster: u64,
}

/// Hands out clusters from the start of the volume.
struct Clusters {
    next: u64,
    total: u64,
}

impl Clusters {
    fn take(&mut self, count: u64) -> Result<Run, String> {
        let lcn = self.next;
        self.next += count;
        if self.next > self.total {
            return Err(format!(
                "the volume holds {} clusters, and its system files need {}",
                self.total, self.next
            ));
        }
        Ok(Run {
            lcn: Some(lcn),
            clusters: count,
        })
    }
}

fn clusters_for(bytes: u64) -> u64 {
    bytes.div_ceil(CLUSTER)
}

fn non_resident(kind: u32, name: &str, run: Run, size: u64) -> Attribute {
    Attribute {
        kind,
        name: name.to_string(),
        value: Value::NonResident {
            runs: vec![run],
            size,
            initialized: size,
            cluster_bytes: CLUSTER,
        },
    }
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// The sequence number of a system record, as Windows gives it.
fn sequence(n: u64) -> u16 {
    match n {
        0 | 1 => 1,
        2..=15 => n as u16,
        _ => 1,
    }
}

/// One file of the volume, before its record is built.
struct File {
    number: u64,
    name: &'static str,
    parent: u64,
    attributes: u32,
    security: u32,
    flags: u16,
    namespace: Namespace,
    /// The sizes that its file name gives.
    sizes: (u64, u64),
    others: Vec<Attribute>,
}

impl File {
    fn file_name(&self, times: Times) -> FileName {
        let name_attributes = self.attributes
            | if self.flags & FLAG_DIRECTORY != 0 {
                fa::DUP_FILE_NAME_INDEX_PRESENT
            } else {
                0
            };
        FileName {
            parent: reference(self.parent, sequence(self.parent)),
            times,
            allocated: self.sizes.0,
            size: self.sizes.1,
            attributes: name_attributes,
            reparse_or_ea: 0,
            name: utf16(self.name),
            namespace: self.namespace,
        }
    }

    fn record(&self, times: Times) -> Record {
        let mut attributes = vec![
            Attribute::resident(
                kind::STANDARD_INFORMATION,
                "",
                standard_information(times, self.attributes, self.security),
            ),
            Attribute::resident(kind::FILE_NAME, "", self.file_name(times).to_bytes()),
        ];
        attributes.extend(self.others.iter().cloned());
        Record {
            number: self.number as u32,
            sequence: sequence(self.number),
            flags: FLAG_IN_USE | self.flags,
            links: 1,
            base: 0,
            attributes,
        }
    }
}

/// The entry of `file` in the index of its directory.
fn name_entry(file: &File, times: Times) -> Entry {
    Entry {
        file: reference(file.number, sequence(file.number)),
        key: file.file_name(times).to_bytes(),
        data: Vec::new(),
    }
}

/// Sort the entries of a directory in the order of NTFS: by the name in
/// upper case, as the up-case table of the volume gives it.
fn sort_names(entries: &mut [Entry], up: &UpCase) {
    let name = |e: &Entry| -> Vec<u16> {
        let units = e.key[0x40] as usize;
        let raw: Vec<u16> = e.key[0x42..0x42 + 2 * units]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        up.name(&raw)
    };
    entries.sort_by_key(name);
}

/// The attributes of an index: its root, and its blocks with their bitmap
/// when the root does not hold it.
fn index_attributes(
    name: &str,
    index: Index,
    clusters: &mut Clusters,
    writes: &mut Vec<(u64, Vec<u8>)>,
) -> Result<Vec<Attribute>, String> {
    let mut out = vec![Attribute::resident(kind::INDEX_ROOT, name, index.root)];
    if !index.blocks.is_empty() {
        let bytes = (index.blocks.len() * INDEX_BLOCK) as u64;
        let run = clusters.take(clusters_for(bytes))?;
        writes.push((run.lcn.unwrap() * CLUSTER, index.blocks.concat()));
        out.push(non_resident(kind::INDEX_ALLOCATION, name, run, bytes));
        out.push(Attribute::resident(kind::BITMAP, name, index.bitmap));
    }
    Ok(out)
}

fn shape(key_kind: u32, collation: u32, root_space: usize) -> Shape {
    Shape {
        key_kind,
        collation,
        block_bytes: INDEX_BLOCK,
        cluster_bytes: CLUSTER as usize,
        root_space,
    }
}

/// The two entries of $Quota:$Q, for the default owner 1 and for the
/// administrators as owner 256, as Windows writes them.
fn quota_entries(time: u64) -> (Vec<Entry>, Vec<Entry>) {
    let admins: Vec<u8> = {
        let mut s = vec![1u8, 2, 0, 0, 0, 0, 0, 5];
        s.extend_from_slice(&32u32.to_le_bytes());
        s.extend_from_slice(&544u32.to_le_bytes());
        s
    };
    let control = |sid: &[u8]| {
        let mut d = Vec::new();
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&1u32.to_le_bytes());
        d.extend_from_slice(&0u64.to_le_bytes());
        d.extend_from_slice(&time.to_le_bytes());
        d.extend_from_slice(&(-1i64).to_le_bytes());
        d.extend_from_slice(&(-1i64).to_le_bytes());
        d.extend_from_slice(&0u64.to_le_bytes());
        d.extend_from_slice(sid);
        d
    };
    let q = vec![
        Entry {
            file: 0,
            key: 1u32.to_le_bytes().to_vec(),
            data: control(&[]),
        },
        Entry {
            file: 0,
            key: 256u32.to_le_bytes().to_vec(),
            data: control(&admins),
        },
    ];
    let o = vec![Entry {
        file: 0,
        key: admins,
        data: 256u32.to_le_bytes().to_vec(),
    }];
    (o, q)
}

/// Lay out an empty volume on a partition of `partition_bytes`, which starts
/// at sector `hidden_sectors` of the drive.
pub(crate) fn format(
    partition_bytes: u64,
    hidden_sectors: u32,
    options: &NtfsOptions,
) -> Result<Formatted, String> {
    let partition_sectors = partition_bytes / SECTOR;
    let total = (partition_sectors - 1) / (CLUSTER / SECTOR);
    let mut clusters = Clusters { next: 0, total };
    let mut writes: Vec<(u64, Vec<u8>)> = Vec::new();
    let times = Times::all(options.time);
    let up = UpCase::recommended();

    // The data of the system files, in the order of their clusters.
    let boot_run = clusters.take(2)?;
    let mirror_run = clusters.take(1)?;
    let upcase: Vec<u8> = (0..=u16::MAX)
        .flat_map(|u| up.unit(u).to_le_bytes())
        .collect();
    let upcase_run = clusters.take(clusters_for(upcase.len() as u64))?;
    writes.push((upcase_run.lcn.unwrap() * CLUSTER, upcase.clone()));
    let attrdef_run = clusters.take(1)?;
    writes.push((attrdef_run.lcn.unwrap() * CLUSTER, attrdef()));
    let descriptors = vec![system_descriptor(READ), system_descriptor(READ_WRITE)];
    let sec = secure(&descriptors);
    let sds_run = clusters.take(clusters_for(sec.sds.len() as u64))?;
    writes.push((sds_run.lcn.unwrap() * CLUSTER, sec.sds.clone()));
    let bitmap_bytes = total.div_ceil(8).div_ceil(8) * 8;
    let bitmap_run = clusters.take(clusters_for(bitmap_bytes))?;
    let mft_bitmap_bytes = MFT_RECORDS / 8;
    let mft_bitmap_run = clusters.take(1)?;
    let log_clusters = (total / 100).div_ceil(16).max(1) * 16;
    let log_clusters = log_clusters.clamp(512, 16384).min(total / 4);
    let log_run = clusters.take(log_clusters)?;
    writes.push((
        log_run.lcn.unwrap() * CLUSTER,
        vec![0xFF; (log_clusters * CLUSTER) as usize],
    ));
    let mft_run = clusters.take(MFT_RECORDS * RECORD_BYTES as u64 / CLUSTER)?;

    let system = fa::HIDDEN | fa::SYSTEM;
    let mut files = vec![
        File {
            number: 0,
            name: "$MFT",
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            sizes: (MFT_RECORDS * 1024, MFT_RECORDS * 1024),
            others: vec![
                non_resident(kind::DATA, "", mft_run, MFT_RECORDS * RECORD_BYTES as u64),
                non_resident(kind::BITMAP, "", mft_bitmap_run, mft_bitmap_bytes),
            ],
        },
        File {
            number: 1,
            name: "$MFTMirr",
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            sizes: (CLUSTER, CLUSTER),
            others: vec![non_resident(kind::DATA, "", mirror_run, CLUSTER)],
        },
        File {
            number: 2,
            name: "$LogFile",
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            sizes: (log_clusters * CLUSTER, log_clusters * CLUSTER),
            others: vec![non_resident(
                kind::DATA,
                "",
                log_run,
                log_clusters * CLUSTER,
            )],
        },
        File {
            number: 3,
            name: "$Volume",
            parent: ROOT,
            attributes: system,
            security: SD_READ_WRITE,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            sizes: (0, 0),
            others: vec![
                Attribute::resident(
                    kind::VOLUME_NAME,
                    "",
                    utf16(&options.label)
                        .iter()
                        .flat_map(|u| u.to_le_bytes())
                        .collect(),
                ),
                Attribute::resident(
                    kind::VOLUME_INFORMATION,
                    "",
                    vec![0, 0, 0, 0, 0, 0, 0, 0, 3, 1, 0, 0],
                ),
                Attribute::resident(kind::DATA, "", Vec::new()),
            ],
        },
        File {
            number: 4,
            name: "$AttrDef",
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            sizes: (CLUSTER, ATTRDEF_BYTES as u64),
            others: vec![non_resident(
                kind::DATA,
                "",
                attrdef_run,
                ATTRDEF_BYTES as u64,
            )],
        },
        File {
            number: 6,
            name: "$Bitmap",
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            sizes: (bitmap_run.clusters * CLUSTER, bitmap_bytes),
            others: vec![non_resident(kind::DATA, "", bitmap_run, bitmap_bytes)],
        },
        File {
            number: 7,
            name: "$Boot",
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            sizes: (2 * CLUSTER, 2 * CLUSTER),
            others: vec![non_resident(kind::DATA, "", boot_run, 2 * CLUSTER)],
        },
        File {
            number: 8,
            name: "$BadClus",
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            sizes: (0, 0),
            others: vec![
                Attribute::resident(kind::DATA, "", Vec::new()),
                Attribute {
                    kind: kind::DATA,
                    name: "$Bad".to_string(),
                    value: Value::NonResident {
                        runs: vec![Run {
                            lcn: None,
                            clusters: total,
                        }],
                        size: total * CLUSTER,
                        initialized: 0,
                        cluster_bytes: CLUSTER,
                    },
                },
            ],
        },
        File {
            number: 10,
            name: "$UpCase",
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            sizes: (upcase.len() as u64, upcase.len() as u64),
            others: vec![non_resident(
                kind::DATA,
                "",
                upcase_run,
                upcase.len() as u64,
            )],
        },
    ];

    // $Secure, with its two view indexes.
    let mut secure_others = vec![non_resident(
        kind::DATA,
        "$SDS",
        sds_run,
        sec.sds.len() as u64,
    )];
    let sdh = build(sec.sdh.clone(), &shape(0, COLLATE_SECURITY_HASH, 300));
    let sii = build(sec.sii.clone(), &shape(0, COLLATE_ULONG, 300));
    secure_others.extend(index_attributes("$SDH", sdh, &mut clusters, &mut writes)?);
    secure_others.extend(index_attributes("$SII", sii, &mut clusters, &mut writes)?);
    files.push(File {
        number: 9,
        name: "$Secure",
        parent: ROOT,
        attributes: system | fa::DUP_VIEW_INDEX_PRESENT,
        security: SD_READ_WRITE,
        flags: FLAG_VIEW_INDEX,
        namespace: Namespace::Win32AndDos,
        sizes: (0, 0),
        others: secure_others,
    });

    // The files in $Extend, and $Extend itself.
    let (quota_o, quota_q) = quota_entries(options.time);
    let mut quota_others = Vec::new();
    quota_others.extend(index_attributes(
        "$O",
        build(quota_o, &shape(0, COLLATE_SID, 300)),
        &mut clusters,
        &mut writes,
    )?);
    quota_others.extend(index_attributes(
        "$Q",
        build(quota_q, &shape(0, COLLATE_ULONG, 400)),
        &mut clusters,
        &mut writes,
    )?);
    let empty_view = |name: &str, clusters: &mut Clusters, writes: &mut Vec<_>| {
        index_attributes(
            name,
            build(Vec::new(), &shape(0, COLLATE_ULONGS, 300)),
            clusters,
            writes,
        )
    };
    let extend_files = vec![
        File {
            number: QUOTA,
            name: "$Quota",
            parent: EXTEND,
            attributes: system | fa::DUP_VIEW_INDEX_PRESENT,
            security: SD_READ_WRITE,
            flags: FLAG_VIEW_INDEX | FLAG_EXTEND_FILE,
            namespace: Namespace::Posix,
            sizes: (0, 0),
            others: quota_others,
        },
        File {
            number: OBJ_ID,
            name: "$ObjId",
            parent: EXTEND,
            attributes: system | fa::DUP_VIEW_INDEX_PRESENT,
            security: SD_READ_WRITE,
            flags: FLAG_VIEW_INDEX | FLAG_EXTEND_FILE,
            namespace: Namespace::Posix,
            sizes: (0, 0),
            others: empty_view("$O", &mut clusters, &mut writes)?,
        },
        File {
            number: REPARSE,
            name: "$Reparse",
            parent: EXTEND,
            attributes: system | fa::DUP_VIEW_INDEX_PRESENT,
            security: SD_READ_WRITE,
            flags: FLAG_VIEW_INDEX | FLAG_EXTEND_FILE,
            namespace: Namespace::Posix,
            sizes: (0, 0),
            others: empty_view("$R", &mut clusters, &mut writes)?,
        },
    ];
    let mut extend_entries: Vec<Entry> =
        extend_files.iter().map(|f| name_entry(f, times)).collect();
    sort_names(&mut extend_entries, &up);
    let extend_index = build(extend_entries, &shape(0x30, COLLATE_FILE_NAME, 600));
    files.push(File {
        number: EXTEND,
        name: "$Extend",
        parent: ROOT,
        attributes: system,
        security: SD_READ_WRITE,
        flags: FLAG_DIRECTORY,
        namespace: Namespace::Win32AndDos,
        sizes: (0, 0),
        others: index_attributes("$I30", extend_index, &mut clusters, &mut writes)?,
    });

    // The root directory, whose index names each system file and itself.
    let mut root = File {
        number: ROOT,
        name: ".",
        parent: ROOT,
        attributes: system,
        security: SD_READ_WRITE,
        flags: FLAG_DIRECTORY,
        namespace: Namespace::Win32AndDos,
        sizes: (0, 0),
        others: Vec::new(),
    };
    let mut root_entries: Vec<Entry> = files.iter().map(|f| name_entry(f, times)).collect();
    root_entries.push(name_entry(&root, times));
    sort_names(&mut root_entries, &up);
    let root_index = build(root_entries, &shape(0x30, COLLATE_FILE_NAME, 500));
    root.others = index_attributes("$I30", root_index, &mut clusters, &mut writes)?;
    files.push(root);
    files.extend(extend_files);

    // The records. 12 to 15 are reserved and in use, and 16 to 23 are free.
    let mut mft = vec![0u8; (MFT_RECORDS as usize) * RECORD_BYTES];
    let mut put = |n: u64, bytes: Vec<u8>| {
        mft[n as usize * RECORD_BYTES..(n as usize + 1) * RECORD_BYTES].copy_from_slice(&bytes);
    };
    for f in &files {
        let r = f
            .record(times)
            .to_bytes()
            .map_err(|e| format!("record {} needs {} bytes", e.number, e.needed))?;
        put(f.number, r);
    }
    for n in 12..16u64 {
        let reserved = Record {
            number: n as u32,
            sequence: sequence(n),
            flags: FLAG_IN_USE,
            links: 0,
            base: 0,
            attributes: vec![
                Attribute::resident(
                    kind::STANDARD_INFORMATION,
                    "",
                    standard_information(times, system, SD_READ),
                ),
                Attribute::resident(kind::DATA, "", Vec::new()),
            ],
        };
        put(
            n,
            reserved
                .to_bytes()
                .map_err(|e| format!("record {}", e.number))?,
        );
    }
    let mut mft_bitmap = vec![0u8; CLUSTER as usize];
    for n in (0..16).chain(QUOTA..FIRST_FREE_RECORD) {
        mft_bitmap[(n / 8) as usize] |= 1 << (n % 8);
    }
    writes.push((mft_bitmap_run.lcn.unwrap() * CLUSTER, mft_bitmap));
    writes.push((
        mirror_run.lcn.unwrap() * CLUSTER,
        mft[..4 * RECORD_BYTES].to_vec(),
    ));
    writes.push((mft_run.lcn.unwrap() * CLUSTER, mft));

    // The bitmap of the volume, now that each cluster of the system files
    // is known. A bit past the last cluster is set, so no file gets it.
    let used = clusters.next;
    let mut bitmap = vec![0u8; (bitmap_run.clusters * CLUSTER) as usize];
    for c in (0..used).chain(total..bitmap_bytes * 8) {
        bitmap[(c / 8) as usize] |= 1 << (c % 8);
    }
    writes.push((bitmap_run.lcn.unwrap() * CLUSTER, bitmap));

    let geometry = Geometry {
        sector_bytes: SECTOR as u32,
        cluster_bytes: CLUSTER as u32,
        partition_sectors,
        hidden_sectors,
        mft_cluster: mft_run.lcn.unwrap(),
        mirror_cluster: mirror_run.lcn.unwrap(),
        index_bytes: INDEX_BLOCK as u32,
        serial: options.serial,
    };
    let boot = boot_sector(&geometry);
    let mut boot_area = vec![0u8; 2 * CLUSTER as usize];
    boot_area[..512].copy_from_slice(&boot);
    writes.push((0, boot_area));
    writes.push(((partition_sectors - 1) * SECTOR, boot));
    writes.sort_by_key(|w| w.0);
    Ok(Formatted {
        geometry,
        writes,
        clusters: total,
        first_free_cluster: used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntfs::record::remove_fixups;

    fn image(bytes: u64) -> (Formatted, Vec<u8>) {
        let f = format(
            bytes,
            2048,
            &NtfsOptions {
                label: "BURNOUT".to_string(),
                serial: 0x1122_3344_5566_7788,
                time: 133_000_000_000_000_000,
            },
        )
        .unwrap();
        let mut img = vec![0u8; bytes as usize];
        for (at, b) in &f.writes {
            img[*at as usize..*at as usize + b.len()].copy_from_slice(b);
        }
        (f, img)
    }

    fn record(img: &[u8], f: &Formatted, n: usize) -> Vec<u8> {
        let at = (f.geometry.mft_cluster * CLUSTER) as usize + n * RECORD_BYTES;
        remove_fixups(&img[at..at + RECORD_BYTES]).unwrap()
    }

    #[test]
    fn the_volume_has_its_boot_sectors_and_its_records() {
        let (f, img) = image(64 * 1024 * 1024);
        assert_eq!(&img[3..11], b"NTFS    ");
        let last = img.len() - 512;
        assert_eq!(&img[last..], &img[..512], "the copy in the last sector");
        for n in 0..16 {
            assert_eq!(&record(&img, &f, n)[0..4], b"FILE", "record {n}");
        }
        for n in 16..24 {
            let at = (f.geometry.mft_cluster * CLUSTER) as usize + n * RECORD_BYTES;
            assert!(
                img[at..at + RECORD_BYTES].iter().all(|b| *b == 0),
                "record {n}"
            );
        }
        for n in 24..27 {
            assert_eq!(&record(&img, &f, n)[0..4], b"FILE", "record {n}");
        }
        // The mirror holds the first four records.
        let mirror = (f.geometry.mirror_cluster * CLUSTER) as usize;
        let mft = (f.geometry.mft_cluster * CLUSTER) as usize;
        assert_eq!(&img[mirror..mirror + 4096], &img[mft..mft + 4096]);
    }

    #[test]
    fn the_bitmap_marks_the_system_clusters_and_nothing_else() {
        let (f, img) = image(64 * 1024 * 1024);
        let bitmap_record = record(&img, &f, 6);
        // The run of $Bitmap is the first in its data attribute.
        let run_at = bitmap_record
            .windows(4)
            .position(|w| w == [0x80, 0, 0, 0])
            .unwrap();
        let runs_offset =
            u16::from_le_bytes([bitmap_record[run_at + 0x20], bitmap_record[run_at + 0x21]])
                as usize;
        let runs = crate::ntfs::runs::decode(&bitmap_record[run_at + runs_offset..]).unwrap();
        let start = (runs[0].lcn.unwrap() * CLUSTER) as usize;
        let bit = |c: u64| img[start + (c / 8) as usize] >> (c % 8) & 1;
        assert!((0..f.first_free_cluster).all(|c| bit(c) == 1));
        assert!((f.first_free_cluster..f.clusters).all(|c| bit(c) == 0));
    }

    #[test]
    fn a_volume_too_small_for_its_system_files_is_refused() {
        let small = format(
            256 * 1024,
            0,
            &NtfsOptions {
                label: String::new(),
                serial: 0,
                time: 0,
            },
        );
        let Err(e) = small else {
            panic!("a volume of 256 KiB was formatted");
        };
        assert!(e.contains("system files need"), "{e}");
    }
}
