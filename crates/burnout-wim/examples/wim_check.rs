//! Decompress the streams of an install image, and check each one against
//! its SHA-1. The image is a WIM or ESD file, or the install image of a
//! Windows ISO.
//!
//!     cargo run --release -p burnout-wim --example wim_check -- <iso, wim or esd> [count]
//!
//! It checks the metadata of each edition, and then the first `count` other
//! streams in the order of the file, 200 by default. A count of `all` checks
//! each stream.

use std::fs::File;
use std::io::{Read, Seek};
use std::process::ExitCode;
use std::time::Instant;

use burnout_core::{Entry, FileSource, TreePath};
use burnout_iso::IsoSource;
use burnout_wim::sha1::{hex, sha1};
use burnout_wim::{read_blobs, read_wim, BlobReader};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(iso) = args.first() else {
        eprintln!("usage: wim_check <iso, wim or esd> [count|all]");
        return ExitCode::from(2);
    };
    let count = match args.get(1).map(String::as_str) {
        None => 200,
        Some("all") => usize::MAX,
        Some(n) => n.parse().expect("a count"),
    };
    match run(iso, count) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("wim_check: {e}");
            ExitCode::FAILURE
        }
    }
}

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

fn run(image: &str, count: usize) -> burnout_core::Result<bool> {
    let mut magic = [0u8; 8];
    let is_wim = File::open(image)?.read_exact(&mut magic).is_ok() && &magic == b"MSWIM\0\0\0";
    // The source of the ISO lives as long as the reader of its install
    // image, so both stay in this function.
    let source;
    let mut file: Box<dyn ReadSeek + '_>;
    let name: String;
    if is_wim {
        name = image.to_string();
        file = Box::new(File::open(image)?);
    } else {
        source = IsoSource::new(File::open(image)?, image)?.expect("an ISO");
        let path = source
            .entries()?
            .into_iter()
            .find_map(|e| match e {
                Entry::File(p, _)
                    if p.as_str().eq_ignore_ascii_case("sources/install.wim")
                        || p.as_str().eq_ignore_ascii_case("sources/install.esd") =>
                {
                    Some(p)
                }
                _ => None,
            })
            .unwrap_or_else(|| TreePath::new("sources/install.wim").unwrap());
        name = path.as_str().to_string();
        file = Box::new(source.open_file(&path)?);
    }
    let wim = read_wim(&mut file, &name)?;
    let mut blobs = read_blobs(&mut file, &wim.header, &name)?;
    // In the order of the file, so each chunk of a solid resource is
    // decompressed once.
    blobs.sort_by_key(|b| {
        let solid = b.solid.as_ref().and_then(|r| r.first()).map(|r| r.offset);
        (
            !b.is_metadata(),
            solid.unwrap_or(b.resource.offset),
            b.resource.offset,
        )
    });
    println!(
        "{}: {} editions, {} streams, {}, chunks of {}",
        name,
        wim.editions.len(),
        blobs.len(),
        wim.header.compression().name(),
        wim.header.chunk_bytes
    );
    let metadata = blobs.iter().filter(|b| b.is_metadata()).count();
    let solid = blobs.iter().filter(|b| b.solid.is_some()).count();
    println!("{solid} streams are in solid resources");
    let mut streams = BlobReader::new();
    let (mut checked, mut bad, mut bytes) = (0usize, 0usize, 0u64);
    let start = Instant::now();
    for blob in blobs.iter().take(metadata.saturating_add(count)) {
        let data = streams.read(&mut file, &wim.header, blob, &name)?;
        bytes += data.len() as u64;
        checked += 1;
        if sha1(&data) != blob.hash {
            bad += 1;
            println!(
                "BAD  stream at byte {}: SHA-1 {} and not {}",
                blob.resource.offset,
                hex(&sha1(&data)),
                hex(&blob.hash)
            );
        }
    }
    let seconds = start.elapsed().as_secs_f64();
    println!(
        "checked {checked} streams ({metadata} of metadata), {bytes} bytes, in {seconds:.1} s; {bad} bad"
    );
    Ok(bad == 0)
}
