//! Decompress the streams of the install image of a Windows ISO, and check
//! each one against its SHA-1.
//!
//!     cargo run --release -p burnout-wim --example wim_check -- <iso> [count]
//!
//! It checks the metadata of each edition, and then the first `count` other
//! streams in the order of the file, 200 by default. A count of `all` checks
//! each stream.

use std::fs::File;
use std::process::ExitCode;
use std::time::Instant;

use burnout_core::{Entry, FileSource, TreePath};
use burnout_iso::IsoSource;
use burnout_wim::sha1::{hex, sha1};
use burnout_wim::{read_blobs, read_resource, read_wim};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(iso) = args.first() else {
        eprintln!("usage: wim_check <iso> [count|all]");
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

fn run(iso: &str, count: usize) -> burnout_core::Result<bool> {
    let source = IsoSource::new(File::open(iso)?, iso)?.expect("an ISO");
    let path = source
        .entries()?
        .into_iter()
        .find_map(|e| match e {
            Entry::File(p, _) if p.as_str().eq_ignore_ascii_case("sources/install.wim") => Some(p),
            _ => None,
        })
        .unwrap_or_else(|| TreePath::new("sources/install.wim").unwrap());
    let wim = read_wim(source.open_file(&path)?, path.as_str())?;
    let mut file = source.open_file(&path)?;
    let mut blobs = read_blobs(&mut file, &wim.header, path.as_str())?;
    blobs.sort_by_key(|b| (!b.is_metadata(), b.resource.offset));
    println!(
        "{}: {} editions, {} streams, {}, chunks of {}",
        path.as_str(),
        wim.editions.len(),
        blobs.len(),
        wim.header.compression().name(),
        wim.header.chunk_bytes
    );
    let metadata = blobs.iter().filter(|b| b.is_metadata()).count();
    let (mut checked, mut bad, mut bytes) = (0usize, 0usize, 0u64);
    let start = Instant::now();
    for blob in blobs.iter().take(metadata.saturating_add(count)) {
        let data = read_resource(&mut file, &wim.header, &blob.resource, path.as_str())?;
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
