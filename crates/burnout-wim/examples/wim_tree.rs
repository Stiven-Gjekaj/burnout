//! Read the tree of each edition of the install image of a Windows ISO.
//!
//!     cargo run --release -p burnout-wim --example wim_tree -- <iso>
//!
//! For each edition it counts the directories and the files, and compares
//! the counts with the ones that the XML gives. It checks that each file
//! with data names a stream of the offset table.

use std::collections::HashSet;
use std::fs::File;
use std::process::ExitCode;

use burnout_core::{Entry, FileSource};
use burnout_iso::IsoSource;
use burnout_wim::{parse_metadata, read_blobs, read_resource, read_wim};

fn main() -> ExitCode {
    let Some(iso) = std::env::args().nth(1) else {
        eprintln!("usage: wim_tree <iso>");
        return ExitCode::from(2);
    };
    match run(&iso) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("wim_tree: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(iso: &str) -> burnout_core::Result<bool> {
    let source = IsoSource::new(File::open(iso)?, iso)?.expect("an ISO");
    let path = source
        .entries()?
        .into_iter()
        .find_map(|e| match e {
            Entry::File(p, _) if p.as_str().eq_ignore_ascii_case("sources/install.wim") => Some(p),
            _ => None,
        })
        .expect("sources/install.wim");
    let wim = read_wim(source.open_file(&path)?, path.as_str())?;
    let mut file = source.open_file(&path)?;
    let blobs = read_blobs(&mut file, &wim.header, path.as_str())?;
    let known: HashSet<[u8; 20]> = blobs.iter().map(|b| b.hash).collect();
    // The metadata resources come in the order of the editions.
    let metas: Vec<_> = blobs.iter().filter(|b| b.is_metadata()).collect();
    let mut metas = metas;
    metas.sort_by_key(|b| b.resource.offset);
    let mut good = true;
    for (edition, meta) in wim.editions.iter().zip(metas) {
        let bytes = read_resource(&mut file, &wim.header, &meta.resource, path.as_str())?;
        let tree = parse_metadata(&bytes).map_err(|e| burnout_core::Error::Image {
            path: path.to_string(),
            detail: e,
        })?;
        // The XML counts a directory that is a reparse point, such as a
        // junction, as a file.
        let dirs = tree
            .entries
            .iter()
            .filter(|e| e.is_directory() && e.reparse_tag.is_none())
            .count() as u64;
        let files = tree.entries.len() as u64 - dirs;
        let missing = tree
            .entries
            .iter()
            .filter(|e| e.data_hash().is_some_and(|h| !known.contains(h)))
            .count();
        let reparse = tree
            .entries
            .iter()
            .filter(|e| e.reparse_tag.is_some())
            .count();
        let streams: usize = tree
            .entries
            .iter()
            .map(|e| e.streams.iter().filter(|s| !s.name.is_empty()).count())
            .sum();
        let links = {
            let mut seen = HashSet::new();
            tree.entries
                .iter()
                .filter(|e| e.hard_link_group != 0 && !seen.insert(e.hard_link_group))
                .count()
        };
        // The root is a directory that the XML does not count.
        let same = edition.dir_count == Some(dirs - 1) && edition.file_count == Some(files);
        good &= same && missing == 0;
        println!(
            "{:>2} {}: {} directories and {} files (XML {:?} and {:?}), {} security descriptors, {} reparse points, {} named streams, {} hard links more, {} without a stream",
            edition.index,
            edition.shown_name(),
            dirs - 1,
            files,
            edition.dir_count,
            edition.file_count,
            tree.security.len(),
            reparse,
            streams,
            links,
            missing
        );
    }
    Ok(good)
}
