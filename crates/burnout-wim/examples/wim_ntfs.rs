//! Apply one edition of the install image of a Windows ISO to a new NTFS
//! volume in a disk image, as Windows To Go will.
//!
//!     cargo run --release -p burnout-wim --example wim_ntfs -- <iso> <edition> <image> <GiB>
//!
//! The image gets an MBR with one NTFS partition from sector 2048. Each
//! file, directory, hard link, reparse point, named stream and security
//! descriptor of the edition goes onto the volume, and each stream is
//! decompressed into its clusters. The image is then for Windows to check.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::process::ExitCode;
use std::time::Instant;

use burnout_core::{Entry, FileSource};
use burnout_iso::IsoSource;
use burnout_layout::ntfs::{format, NewFile, NtfsOptions, Run, Times, CLUSTER};
use burnout_wim::sha1::Digest;
use burnout_wim::{copy_resource, parse_metadata, read_blobs, read_resource, read_wim, Blob};

/// The first sector of the NTFS partition. WIM_NTFS_START_MIB moves it, to
/// leave room for an EFI partition in front of it.
fn start_sector() -> u64 {
    std::env::var("WIM_NTFS_START_MIB")
        .ok()
        .and_then(|m| m.parse::<u64>().ok())
        .map_or(2048, |m| m * 2048)
}
/// The attributes of Windows that a file keeps on the new volume. Sparse,
/// compressed and encrypted need attributes of their own, which the writer
/// does not make yet.
const KEPT: u32 = 0x0001 | 0x0002 | 0x0004 | 0x0010 | 0x0020 | 0x0080 | 0x0100 | 0x1000 | 0x2000;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 4 {
        eprintln!("usage: wim_ntfs <iso> <edition> <image> <GiB>");
        return ExitCode::from(2);
    }
    match run(
        &args[0],
        args[1].parse().unwrap(),
        &args[2],
        args[3].parse().unwrap(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wim_ntfs: {e}");
            ExitCode::FAILURE
        }
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn run(iso: &str, edition: usize, image: &str, gib: u64) -> Result<(), String> {
    let start = Instant::now();
    let source = IsoSource::new(File::open(iso).map_err(err)?, iso)
        .map_err(err)?
        .ok_or("not an ISO")?;
    let path = source
        .entries()
        .map_err(err)?
        .into_iter()
        .find_map(|e| match e {
            Entry::File(p, _) if p.as_str().eq_ignore_ascii_case("sources/install.wim") => Some(p),
            _ => None,
        })
        .ok_or("no sources/install.wim")?;
    let name = path.as_str();
    let wim = read_wim(source.open_file(&path).map_err(err)?, name).map_err(err)?;
    let mut wim_file = source.open_file(&path).map_err(err)?;
    let blobs = read_blobs(&mut wim_file, &wim.header, name).map_err(err)?;
    let by_hash: HashMap<Digest, &Blob> = blobs.iter().map(|b| (b.hash, b)).collect();
    let mut metas: Vec<&Blob> = blobs.iter().filter(|b| b.is_metadata()).collect();
    metas.sort_by_key(|b| b.resource.offset);
    let meta = metas.get(edition - 1).ok_or("no such edition")?;
    let bytes = read_resource(&mut wim_file, &wim.header, &meta.resource, name).map_err(err)?;
    let tree = parse_metadata(&bytes)?;
    println!(
        "edition {edition}, {}: {} entries, {} descriptors",
        wim.editions[edition - 1].shown_name(),
        tree.entries.len(),
        tree.security.len()
    );

    // The new files, in the order of the tree. A hard link becomes a
    // second name of the file that it links to.
    let mut list: Vec<NewFile> = Vec::new();
    let mut index_of: HashMap<&str, usize> = HashMap::new();
    let mut by_group: HashMap<u64, usize> = HashMap::new();
    let mut fills: Vec<(u64, Digest, usize, Option<usize>)> = Vec::new();
    for d in tree.entries.iter().skip(1) {
        let (parent_path, leaf) = d.path.rsplit_once('/').unwrap_or(("", &d.path));
        let parent = if parent_path.is_empty() {
            None
        } else {
            Some(
                *index_of
                    .get(parent_path)
                    .ok_or_else(|| format!("no parent for {}", d.path))?,
            )
        };
        if d.hard_link_group != 0 && !d.is_directory() {
            if let Some(&first) = by_group.get(&d.hard_link_group) {
                list[first].links.push((parent, leaf.to_string()));
                continue;
            }
        }
        let size_of = |h: &Digest| by_hash.get(h).map(|b| b.resource.original_bytes);
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
        if let Some(tag) = d.reparse_tag {
            let body = match &data {
                Some(h) => {
                    let b = by_hash.get(h).ok_or("a reparse point names no stream")?;
                    read_resource(&mut wim_file, &wim.header, &b.resource, name).map_err(err)?
                }
                None => Vec::new(),
            };
            let mut rp = tag.to_le_bytes().to_vec();
            rp.extend_from_slice(&(body.len() as u16).to_le_bytes());
            rp.extend_from_slice(&[0, 0]);
            rp.extend_from_slice(&body);
            f.reparse = Some(rp);
        } else if let Some(h) = data {
            f.size = size_of(&h).ok_or_else(|| format!("{} names no stream", d.path))?;
        }
        let i = list.len();
        if let (Some(h), None, false) = (data, d.reparse_tag, d.is_directory()) {
            fills.push((0, h, i, None));
        }
        for s in d.streams.iter().filter(|s| !s.name.is_empty()) {
            let size = if s.hash == [0; 20] {
                0
            } else {
                size_of(&s.hash).ok_or("a stream names no blob")?
            };
            if size > 0 {
                fills.push((0, s.hash, i, Some(f.streams.len())));
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
    let links: usize = list.iter().map(|f| f.links.len()).sum();
    println!(
        "{} files and directories, {links} more names, {} streams to fill",
        list.len(),
        fills.len()
    );

    let disk = gib * 1024 * 1024 * 1024;
    // The last MiB stays free, so a GPT can keep its copy there.
    let partition = disk - start_sector() * 512 - 1024 * 1024;
    let f = format(
        partition,
        start_sector() as u32,
        &NtfsOptions {
            label: "WINDOWS".into(),
            serial: 0x5EED_0000_B0B0_0001,
            time: 134_070_000_000_000_000,
        },
        &list,
        &tree.security,
    )?;
    println!(
        "laid out in {:.1} s, {} clusters used",
        start.elapsed().as_secs_f64(),
        f.first_free_cluster
    );

    let out = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(image)
        .map_err(err)?;
    out.set_len(disk).map_err(err)?;
    let mut out = BufWriter::with_capacity(8 << 20, out);
    let mut mbr = vec![0u8; 512];
    mbr[446 + 4] = 0x07;
    mbr[446 + 8..446 + 12].copy_from_slice(&(start_sector() as u32).to_le_bytes());
    mbr[446 + 12..446 + 16].copy_from_slice(&((partition / 512) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    out.write_all(&mbr).map_err(err)?;
    let base = start_sector() * 512;
    for (at, b) in &f.writes {
        out.seek(SeekFrom::Start(base + at)).map_err(err)?;
        out.write_all(b).map_err(err)?;
    }
    // Each stream, in the order of the WIM file, into its clusters.
    for fill in fills.iter_mut() {
        fill.0 = by_hash[&fill.1].resource.offset;
    }
    fills.sort();
    let mut written = 0u64;
    for (_, hash, i, stream) in &fills {
        let run: Run = match stream {
            None => f.placements[*i],
            Some(s) => f.stream_placements[*i][*s],
        }
        .ok_or("a stream with bytes has no run")?;
        out.seek(SeekFrom::Start(base + run.lcn.unwrap() * CLUSTER))
            .map_err(err)?;
        let blob = by_hash[hash];
        copy_resource(&mut wim_file, &wim.header, &blob.resource, name, &mut out).map_err(err)?;
        written += blob.resource.original_bytes;
    }
    out.flush().map_err(err)?;
    println!(
        "wrote {written} bytes of {} streams in {:.1} s",
        fills.len(),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}
