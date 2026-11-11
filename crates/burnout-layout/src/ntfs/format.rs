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

/// A file or a directory to put on a new volume.
#[derive(Clone, Debug)]
pub(crate) struct NewFile {
    /// The directory that holds it, as an index into the list, or `None`
    /// for the root. A directory comes before what it holds.
    pub parent: Option<usize>,
    pub name: String,
    pub directory: bool,
    /// The bytes of a file. The caller writes them into its run.
    pub size: u64,
    /// The attributes of Windows. The directory flag goes where NTFS keeps it.
    pub attributes: u32,
    pub times: Times,
    /// The descriptor, as an index into the descriptors of the files.
    pub security: Option<usize>,
    /// The other names of the file, each in its directory: hard links.
    pub links: Vec<(Option<usize>, String)>,
    /// The reparse data, from the tag on, for a reparse point.
    pub reparse: Option<Vec<u8>>,
    /// The named streams of the file, each with its size.
    pub streams: Vec<(String, u64)>,
}

/// A new volume: the bytes to write, each at its byte of the volume.
#[derive(Clone, Debug)]
pub(crate) struct Formatted {
    pub geometry: Geometry,
    pub writes: Vec<(u64, Vec<u8>)>,
    /// The clusters of the volume, and the first one that no file holds.
    pub clusters: u64,
    pub first_free_cluster: u64,
    /// The clusters of the data of each new file, in the order of the list.
    /// A directory and an empty file have none.
    pub placements: Vec<Option<Run>>,
    /// The clusters of each named stream of each new file, in the order
    /// of its streams. An empty stream has none.
    pub stream_placements: Vec<Vec<Option<Run>>>,
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
    name: String,
    times: Times,
    parent: u64,
    attributes: u32,
    security: u32,
    flags: u16,
    namespace: Namespace,
    /// The sizes that its file name gives.
    sizes: (u64, u64),
    others: Vec<Attribute>,
    /// Its other names, each with the record of its directory.
    extra_names: Vec<(u64, String)>,
    /// The tag of a reparse point, or 0.
    reparse_tag: u32,
}

impl File {
    fn file_name(&self) -> FileName {
        self.file_name_as(self.parent, &self.name, self.namespace)
    }

    /// The file name with this parent and name, with the sizes and the
    /// attributes of the file.
    fn file_name_as(&self, parent: u64, name: &str, namespace: Namespace) -> FileName {
        let times = self.times;
        let name_attributes = self.attributes
            | if self.flags & FLAG_DIRECTORY != 0 {
                fa::DUP_FILE_NAME_INDEX_PRESENT
            } else {
                0
            };
        FileName {
            parent: reference(parent, sequence(parent)),
            times,
            allocated: self.sizes.0,
            size: self.sizes.1,
            attributes: name_attributes,
            reparse_or_ea: self.reparse_tag,
            name: utf16(name),
            namespace,
        }
    }

    fn record(&self) -> Record {
        let times = self.times;
        let mut attributes = vec![
            Attribute::resident(
                kind::STANDARD_INFORMATION,
                "",
                standard_information(times, self.attributes, self.security),
            ),
            Attribute::resident(kind::FILE_NAME, "", self.file_name().to_bytes()),
        ];
        for (parent, name) in &self.extra_names {
            let link = self.file_name_as(*parent, name, Namespace::Posix);
            attributes.push(Attribute::resident(kind::FILE_NAME, "", link.to_bytes()));
        }
        attributes.extend(self.others.iter().cloned());
        Record {
            number: self.number as u32,
            sequence: sequence(self.number),
            flags: FLAG_IN_USE | self.flags,
            links: 1 + self.extra_names.len() as u16,
            base: 0,
            attributes,
        }
    }
}

/// The namespace of a new name: both, when the name is a valid 8.3 name
/// as DOS writes it, and POSIX else. A Win32 name needs a DOS name beside
/// it: chkdsk of Windows 11 finds a "minor file name error" in a file that
/// has a Win32 name alone, measured on 2026-11-11. Windows puts the files
/// of $Extend in POSIX too.
fn namespace_for(name: &str) -> Namespace {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, Some(e)),
        None => (name, None),
    };
    let part_ok = |p: &str, max: usize| {
        !p.is_empty()
            && p.len() <= max
            && p.bytes().all(|b| {
                b.is_ascii_uppercase() || b.is_ascii_digit() || b"_-$~!#%&@^()'{}".contains(&b)
            })
    };
    let dos = part_ok(base, 8) && ext.map_or(true, |e| part_ok(e, 3));
    if dos {
        Namespace::Win32AndDos
    } else {
        Namespace::Posix
    }
}

/// The bytes that $INDEX_ROOT may take in the record of a directory whose
/// name has `units` UTF-16 units, beside its other attributes.
fn root_space(units: usize) -> usize {
    let fixed =
        FIRST_RECORD_BYTES + (0x18 + 72) + align8(0x18 + 0x42 + 2 * units) + 0x50 + 0x28 + 0x20;
    RECORD_BYTES.saturating_sub(fixed)
}

/// The header of a record and its end marker.
const FIRST_RECORD_BYTES: usize = 0x38 + 8;

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// The entries of each name of `file`, with the record of the directory
/// that holds each name.
fn name_entries(file: &File) -> Vec<(u64, Entry)> {
    let mut out = vec![(file.parent, name_entry(file))];
    for (parent, name) in &file.extra_names {
        out.push((
            *parent,
            Entry {
                file: reference(file.number, sequence(file.number)),
                key: file
                    .file_name_as(*parent, name, Namespace::Posix)
                    .to_bytes(),
                data: Vec::new(),
            },
        ));
    }
    out
}

/// The entry of `file` in the index of its directory.
fn name_entry(file: &File) -> Entry {
    Entry {
        file: reference(file.number, sequence(file.number)),
        key: file.file_name().to_bytes(),
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
    new_files: &[NewFile],
    new_descriptors: &[Vec<u8>],
) -> Result<Formatted, String> {
    let mft_records = (FIRST_FREE_RECORD + new_files.len() as u64)
        .div_ceil(MFT_RECORDS)
        .max(1)
        * MFT_RECORDS;
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
    let mut descriptors = vec![system_descriptor(READ), system_descriptor(READ_WRITE)];
    descriptors.extend(new_descriptors.iter().cloned());
    let sec = secure(&descriptors);
    let sds_run = clusters.take(clusters_for(sec.sds.len() as u64))?;
    writes.push((sds_run.lcn.unwrap() * CLUSTER, sec.sds.clone()));
    let bitmap_bytes = total.div_ceil(8).div_ceil(8) * 8;
    let bitmap_run = clusters.take(clusters_for(bitmap_bytes))?;
    let mft_bitmap_bytes = mft_records.div_ceil(8).div_ceil(8) * 8;
    let mft_bitmap_run = clusters.take(clusters_for(mft_bitmap_bytes))?;
    let log_clusters = (total / 100).div_ceil(16).max(1) * 16;
    let log_clusters = log_clusters.clamp(512, 16384).min(total / 4);
    let log_run = clusters.take(log_clusters)?;
    writes.push((
        log_run.lcn.unwrap() * CLUSTER,
        vec![0xFF; (log_clusters * CLUSTER) as usize],
    ));
    let mft_run = clusters.take(mft_records * RECORD_BYTES as u64 / CLUSTER)?;

    let system = fa::HIDDEN | fa::SYSTEM;
    let mut files = vec![
        File {
            number: 0,
            name: "$MFT".to_string(),
            times,
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            extra_names: Vec::new(),
            reparse_tag: 0,
            sizes: (mft_records * 1024, mft_records * 1024),
            others: vec![
                non_resident(kind::DATA, "", mft_run, mft_records * RECORD_BYTES as u64),
                non_resident(kind::BITMAP, "", mft_bitmap_run, mft_bitmap_bytes),
            ],
        },
        File {
            number: 1,
            name: "$MFTMirr".to_string(),
            times,
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            extra_names: Vec::new(),
            reparse_tag: 0,
            sizes: (CLUSTER, CLUSTER),
            others: vec![non_resident(kind::DATA, "", mirror_run, CLUSTER)],
        },
        File {
            number: 2,
            name: "$LogFile".to_string(),
            times,
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            extra_names: Vec::new(),
            reparse_tag: 0,
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
            name: "$Volume".to_string(),
            times,
            parent: ROOT,
            attributes: system,
            security: SD_READ_WRITE,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            extra_names: Vec::new(),
            reparse_tag: 0,
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
            name: "$AttrDef".to_string(),
            times,
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            extra_names: Vec::new(),
            reparse_tag: 0,
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
            name: "$Bitmap".to_string(),
            times,
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            extra_names: Vec::new(),
            reparse_tag: 0,
            sizes: (bitmap_run.clusters * CLUSTER, bitmap_bytes),
            others: vec![non_resident(kind::DATA, "", bitmap_run, bitmap_bytes)],
        },
        File {
            number: 7,
            name: "$Boot".to_string(),
            times,
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            extra_names: Vec::new(),
            reparse_tag: 0,
            sizes: (2 * CLUSTER, 2 * CLUSTER),
            others: vec![non_resident(kind::DATA, "", boot_run, 2 * CLUSTER)],
        },
        File {
            number: 8,
            name: "$BadClus".to_string(),
            times,
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            extra_names: Vec::new(),
            reparse_tag: 0,
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
            name: "$UpCase".to_string(),
            times,
            parent: ROOT,
            attributes: system,
            security: SD_READ,
            flags: 0,
            namespace: Namespace::Win32AndDos,
            extra_names: Vec::new(),
            reparse_tag: 0,
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
        name: "$Secure".to_string(),
        times,
        parent: ROOT,
        attributes: system | fa::DUP_VIEW_INDEX_PRESENT,
        security: SD_READ_WRITE,
        flags: FLAG_VIEW_INDEX,
        namespace: Namespace::Win32AndDos,
        extra_names: Vec::new(),
        reparse_tag: 0,
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
    // The new files: a record each from record 27 on, the data of each file
    // in clusters of its own, and an index for each directory.
    let mut user: Vec<File> = Vec::with_capacity(new_files.len());
    let mut placements = Vec::with_capacity(new_files.len());
    let mut reparse_entries: Vec<Entry> = Vec::new();
    let mut stream_placements = Vec::with_capacity(new_files.len());
    let record_of = |p: Option<usize>| p.map_or(ROOT, |p| FIRST_FREE_RECORD + p as u64);
    for (i, f) in new_files.iter().enumerate() {
        let names =
            std::iter::once((f.parent, &f.name)).chain(f.links.iter().map(|(p, n)| (*p, n)));
        for (parent, name) in names {
            if name.encode_utf16().count() > 255 || name.is_empty() {
                return Err(format!("the name {name:?} does not fit NTFS"));
            }
            if let Some(p) = parent {
                if p >= new_files.len() || p == i || !new_files[p].directory {
                    return Err(format!("{name:?} names {p}, which is not a directory"));
                }
            }
        }
        let extra_names: Vec<(u64, String)> = f
            .links
            .iter()
            .map(|(p, n)| (record_of(*p), n.clone()))
            .collect();
        let mut attributes = f.attributes & !fa::DIRECTORY;
        let mut reparse_tag = 0;
        let (others, sizes, run) = if f.directory {
            (Vec::new(), (0, 0), None)
        } else if f.size == 0 {
            (
                vec![Attribute::resident(kind::DATA, "", Vec::new())],
                (0, 0),
                None,
            )
        } else {
            let run = clusters.take(clusters_for(f.size))?;
            (
                vec![non_resident(kind::DATA, "", run, f.size)],
                (run.clusters * CLUSTER, f.size),
                Some(run),
            )
        };
        let mut others = others;
        let mut runs = Vec::with_capacity(f.streams.len());
        for (name, size) in &f.streams {
            if name.is_empty() || name.encode_utf16().count() > 255 {
                return Err(format!(
                    "the stream {name:?} of {:?} does not fit NTFS",
                    f.name
                ));
            }
            if *size == 0 {
                others.push(Attribute::resident(kind::DATA, name, Vec::new()));
                runs.push(None);
            } else {
                let run = clusters.take(clusters_for(*size))?;
                others.push(non_resident(kind::DATA, name, run, *size));
                runs.push(Some(run));
            }
        }
        stream_placements.push(runs);
        if let Some(data) = &f.reparse {
            if data.len() < 8 {
                return Err(format!(
                    "the reparse data of {:?} is shorter than its header",
                    f.name
                ));
            }
            reparse_tag = u32::from_le_bytes(data[0..4].try_into().unwrap());
            attributes |= fa::REPARSE_POINT;
            others.push(Attribute::resident(kind::REPARSE_POINT, "", data.clone()));
            let file = reference(FIRST_FREE_RECORD + i as u64, 1);
            let mut key = reparse_tag.to_le_bytes().to_vec();
            key.extend_from_slice(&file.to_le_bytes());
            reparse_entries.push(Entry {
                file: 0,
                key,
                data: Vec::new(),
            });
        }
        placements.push(run);
        user.push(File {
            number: FIRST_FREE_RECORD + i as u64,
            name: f.name.clone(),
            times: f.times,
            parent: record_of(f.parent),
            attributes: if attributes == 0 {
                fa::NORMAL
            } else {
                attributes
            },
            security: f
                .security
                .map_or(SD_READ_WRITE, |s| FIRST_ID + 2 + s as u32),
            flags: if f.directory { FLAG_DIRECTORY } else { 0 },
            // A file has one DOS name at most, so a file with hard links
            // keeps each of its names in POSIX.
            namespace: if f.links.is_empty() {
                namespace_for(&f.name)
            } else {
                Namespace::Posix
            },
            sizes,
            others,
            extra_names,
            reparse_tag,
        });
    }
    // $R keeps its entries in the order of the tag, then of the file, each
    // as 32-bit numbers.
    reparse_entries.sort_by_key(|e| {
        let w = |i: usize| u32::from_le_bytes(e.key[i..i + 4].try_into().unwrap());
        (w(0), w(4), w(8))
    });
    let mut by_parent: std::collections::BTreeMap<u64, Vec<Entry>> = Default::default();
    for (parent, entry) in user.iter().flat_map(name_entries) {
        by_parent.entry(parent).or_default().push(entry);
    }
    for i in 0..new_files.len() {
        if !new_files[i].directory {
            continue;
        }
        let mut entries = by_parent
            .remove(&(FIRST_FREE_RECORD + i as u64))
            .unwrap_or_default();
        sort_names(&mut entries, &up);
        let units = user[i].name.encode_utf16().count();
        let index = build(entries, &shape(0x30, COLLATE_FILE_NAME, root_space(units)));
        user[i]
            .others
            .extend(index_attributes("$I30", index, &mut clusters, &mut writes)?);
    }

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
            name: "$Quota".to_string(),
            times,
            parent: EXTEND,
            attributes: system | fa::DUP_VIEW_INDEX_PRESENT,
            security: SD_READ_WRITE,
            flags: FLAG_VIEW_INDEX | FLAG_EXTEND_FILE,
            namespace: Namespace::Posix,
            extra_names: Vec::new(),
            reparse_tag: 0,
            sizes: (0, 0),
            others: quota_others,
        },
        File {
            number: OBJ_ID,
            name: "$ObjId".to_string(),
            times,
            parent: EXTEND,
            attributes: system | fa::DUP_VIEW_INDEX_PRESENT,
            security: SD_READ_WRITE,
            flags: FLAG_VIEW_INDEX | FLAG_EXTEND_FILE,
            namespace: Namespace::Posix,
            extra_names: Vec::new(),
            reparse_tag: 0,
            sizes: (0, 0),
            others: empty_view("$O", &mut clusters, &mut writes)?,
        },
        File {
            number: REPARSE,
            name: "$Reparse".to_string(),
            times,
            parent: EXTEND,
            attributes: system | fa::DUP_VIEW_INDEX_PRESENT,
            security: SD_READ_WRITE,
            flags: FLAG_VIEW_INDEX | FLAG_EXTEND_FILE,
            namespace: Namespace::Posix,
            extra_names: Vec::new(),
            reparse_tag: 0,
            sizes: (0, 0),
            others: index_attributes(
                "$R",
                build(reparse_entries, &shape(0, COLLATE_ULONGS, 300)),
                &mut clusters,
                &mut writes,
            )?,
        },
    ];
    let mut extend_entries: Vec<Entry> = extend_files.iter().map(name_entry).collect();
    sort_names(&mut extend_entries, &up);
    let extend_index = build(extend_entries, &shape(0x30, COLLATE_FILE_NAME, 600));
    files.push(File {
        number: EXTEND,
        name: "$Extend".to_string(),
        times,
        parent: ROOT,
        attributes: system,
        security: SD_READ_WRITE,
        flags: FLAG_DIRECTORY,
        namespace: Namespace::Win32AndDos,
        extra_names: Vec::new(),
        reparse_tag: 0,
        sizes: (0, 0),
        others: index_attributes("$I30", extend_index, &mut clusters, &mut writes)?,
    });

    // The root directory, whose index names each system file and itself.
    let mut root = File {
        number: ROOT,
        name: ".".to_string(),
        times,
        parent: ROOT,
        attributes: system,
        security: SD_READ_WRITE,
        flags: FLAG_DIRECTORY,
        namespace: Namespace::Win32AndDos,
        extra_names: Vec::new(),
        reparse_tag: 0,
        sizes: (0, 0),
        others: Vec::new(),
    };
    let mut root_entries: Vec<Entry> = files.iter().map(name_entry).collect();
    root_entries.push(name_entry(&root));
    root_entries.extend(by_parent.remove(&ROOT).unwrap_or_default());
    sort_names(&mut root_entries, &up);
    let root_index = build(root_entries, &shape(0x30, COLLATE_FILE_NAME, root_space(1)));
    root.others = index_attributes("$I30", root_index, &mut clusters, &mut writes)?;
    files.push(root);
    files.extend(extend_files);
    files.extend(user);

    // The records. 12 to 15 are reserved and in use, and 16 to 23 are free.
    let mut mft = vec![0u8; (mft_records as usize) * RECORD_BYTES];
    let mut put = |n: u64, bytes: Vec<u8>| {
        mft[n as usize * RECORD_BYTES..(n as usize + 1) * RECORD_BYTES].copy_from_slice(&bytes);
    };
    for f in &files {
        let r = f
            .record()
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
    let mut mft_bitmap = vec![0u8; (clusters_for(mft_bitmap_bytes) * CLUSTER) as usize];
    let last = FIRST_FREE_RECORD + new_files.len() as u64;
    for n in (0..16).chain(QUOTA..last) {
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
        placements,
        stream_placements,
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
            &[],
            &[],
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
            &[],
            &[],
        );
        let Err(e) = small else {
            panic!("a volume of 256 KiB was formatted");
        };
        assert!(e.contains("system files need"), "{e}");
    }

    fn new_file(parent: Option<usize>, name: &str, directory: bool, size: u64) -> NewFile {
        NewFile {
            parent,
            name: name.to_string(),
            directory,
            size,
            attributes: if directory {
                fa::DIRECTORY
            } else {
                fa::ARCHIVE
            },
            times: Times::all(5),
            security: None,
            links: Vec::new(),
            reparse: None,
            streams: Vec::new(),
        }
    }

    #[test]
    fn new_files_get_records_runs_and_index_entries() {
        let mut list = vec![
            new_file(None, "Windows", true, 0),
            new_file(Some(0), "notepad.exe", false, 10_000),
            new_file(None, "EMPTY.TXT", false, 0),
        ];
        for i in 0..300 {
            list.push(new_file(Some(0), &format!("file{i:03}.dll"), false, 100));
        }
        let f = format(
            64 * 1024 * 1024,
            0,
            &NtfsOptions {
                label: String::new(),
                serial: 1,
                time: 1,
            },
            &list,
            &[vec![
                1, 0, 4, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ]],
        )
        .unwrap();
        assert_eq!(f.placements.len(), 303);
        assert_eq!(f.placements[0], None);
        assert_eq!(f.placements[1].unwrap().clusters, 3);
        assert_eq!(f.placements[2], None);
        let mut img = vec![0u8; 64 * 1024 * 1024];
        for (at, b) in &f.writes {
            img[*at as usize..*at as usize + b.len()].copy_from_slice(b);
        }
        // Record 27 is the directory, with an index in blocks.
        let r = record(&img, &f, 27);
        assert_eq!(u16::from_le_bytes([r[0x16], r[0x17]]), 3);
        assert!(
            r.windows(4).any(|w| w == [0xA0, 0, 0, 0]),
            "the directory has blocks"
        );
        // Each run of data lies past the system files, apart from the others.
        let mut runs: Vec<Run> = f.placements.iter().flatten().copied().collect();
        runs.sort_by_key(|r| r.lcn);
        assert!(runs
            .windows(2)
            .all(|w| w[0].lcn.unwrap() + w[0].clusters <= w[1].lcn.unwrap()));
        assert!(runs.last().map_or(true, |r| r.lcn.unwrap() + r.clusters
            <= f.first_free_cluster));
    }

    #[test]
    fn a_name_that_fits_dos_gets_both_namespaces_and_a_long_one_posix() {
        assert_eq!(namespace_for("BOOTMGR"), Namespace::Win32AndDos);
        assert_eq!(namespace_for("EMPTY.TXT"), Namespace::Win32AndDos);
        assert_eq!(namespace_for("notepad.exe"), Namespace::Posix);
        assert_eq!(namespace_for("LONGERNAME.TXT"), Namespace::Posix);
        assert_eq!(namespace_for("A.HTML"), Namespace::Posix);
        assert_eq!(namespace_for("A B.TXT"), Namespace::Posix);
    }

    #[test]
    fn a_name_must_be_in_a_directory() {
        let list = [
            new_file(Some(1), "a", false, 1),
            new_file(None, "f", false, 0),
        ];
        let r = format(
            64 * 1024 * 1024,
            0,
            &NtfsOptions {
                label: String::new(),
                serial: 1,
                time: 1,
            },
            &list,
            &[],
        );
        let Err(e) = r else {
            panic!("a name in a file was taken")
        };
        assert!(e.contains("which is not a directory"), "{e}");
    }

    pub(super) fn junction(target: &str) -> Vec<u8> {
        let subst: Vec<u8> = format!("\\??\\{target}")
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let print: Vec<u8> = target
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let mut body = Vec::new();
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&(subst.len() as u16).to_le_bytes());
        body.extend_from_slice(&((subst.len() + 2) as u16).to_le_bytes());
        body.extend_from_slice(&(print.len() as u16).to_le_bytes());
        body.extend_from_slice(&subst);
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&print);
        body.extend_from_slice(&[0, 0]);
        let mut data = 0xA000_0003u32.to_le_bytes().to_vec();
        data.extend_from_slice(&(body.len() as u16).to_le_bytes());
        data.extend_from_slice(&[0, 0]);
        data.extend_from_slice(&body);
        data
    }

    #[test]
    fn a_hard_link_and_a_junction_get_their_names_and_their_reparse_entry() {
        let mut linked = new_file(Some(0), "hosts", false, 10);
        linked.links = vec![
            (None, "hosts.txt".to_string()),
            (Some(0), "HOSTS2".to_string()),
        ];
        let mut docs = new_file(None, "Documents and Settings", true, 0);
        docs.reparse = Some(junction("C:\\Users"));
        docs.attributes |= fa::HIDDEN | fa::SYSTEM;
        let list = vec![new_file(None, "etc", true, 0), linked, docs];
        let f = format(
            64 * 1024 * 1024,
            0,
            &NtfsOptions {
                label: String::new(),
                serial: 1,
                time: 1,
            },
            &list,
            &[],
        )
        .unwrap();
        let mut img = vec![0u8; 64 * 1024 * 1024];
        for (at, b) in &f.writes {
            img[*at as usize..*at as usize + b.len()].copy_from_slice(b);
        }
        let r = record(&img, &f, 28);
        assert_eq!(u16::from_le_bytes([r[0x12], r[0x13]]), 3, "three names");
        let names = r.windows(4).filter(|w| *w == [0x30, 0, 0, 0]).count();
        assert!(names >= 3, "{names} file names");
        let d = record(&img, &f, 29);
        assert!(
            d.windows(4).any(|w| w == [0xC0, 0, 0, 0]),
            "a reparse point"
        );
        // $Reparse:$R holds the tag and the file.
        let rp = record(&img, &f, 26);
        assert!(
            rp.windows(4).any(|w| w == 0xA000_0003u32.to_le_bytes()),
            "the tag in $R"
        );
    }
}
