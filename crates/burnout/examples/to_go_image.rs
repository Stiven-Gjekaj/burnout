//! Write a drive of Windows To Go into an image file, with the code of
//! `burnout write --to-go`, and read it back.
//!
//!     cargo run --release -p burnout --example to_go_image -- <iso or wim> <edition> <image> <GiB>
//!
//! A test that cannot attach a virtual disk, because it has no privilege,
//! uses this. A VM then starts the image. The image is raw, with sectors of
//! 512 bytes, and the file is sparse where the host allows it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use burnout::to_go;
use burnout_core::{FileTarget, Silent};
use burnout_iso::IsoSource;
use burnout_layout::{InstallImage, WindowsTree};
use burnout_wim::{read_wim, Compression};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 4 {
        eprintln!("usage: to_go_image <iso or wim> <edition> <image> <GiB>");
        return ExitCode::from(2);
    }
    let Ok(gib) = args[3].parse::<u64>() else {
        eprintln!("to_go_image: {} is not a number of GiB", args[3]);
        return ExitCode::from(2);
    };
    match run(&args[0], &args[1], Path::new(&args[2]), gib) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("to_go_image: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(image: &str, edition: &str, out: &Path, gib: u64) -> Result<(), String> {
    let start = Instant::now();
    let err = |e: burnout_core::Error| e.to_string();
    let mut file = File::open(image).map_err(|e| format!("{image}: {e}"))?;
    let mut magic = [0u8; 8];
    let is_wim = file.read_exact(&mut magic).is_ok() && &magic == b"MSWIM\0\0\0";
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;

    // The source of the ISO lives as long as the reader of its install
    // image, so both stay in this function.
    let source;
    let mut reader: Box<dyn ReadSeek + '_>;
    let name;
    if is_wim {
        name = image.to_string();
        reader = Box::new(file);
    } else {
        source = IsoSource::new(file, image)
            .map_err(err)?
            .ok_or("neither an ISO nor a WIM file")?;
        let tree = WindowsTree::new(&source).map_err(err)?;
        if tree.image != InstallImage::Wim {
            return Err(format!(
                "it holds {}, which LZMS compresses",
                tree.image_path()
            ));
        }
        name = tree.image_path().to_string();
        reader = Box::new(source.open_file(tree.image_path()).map_err(err)?);
    }
    let wim = read_wim(&mut reader, &name).map_err(err)?;
    if wim.header.compression() == Compression::Lzms {
        return Err("LZMS compresses it".to_string());
    }
    let chosen = wim
        .find_edition(edition)
        .ok_or_else(|| format!("no edition {edition}"))?;
    let position = wim
        .editions
        .iter()
        .position(|e| e.index == chosen.index)
        .expect("the edition is in the list")
        + 1;

    let bytes = gib * 1024 * 1024 * 1024;
    let seed = format!("{}/{:?}", out.display(), std::time::SystemTime::now());
    let plan = to_go::plan(&mut reader, &name, &wim, position, bytes, &seed).map_err(err)?;
    println!(
        "{}: {} files and directories, {} more names, {} bytes of streams, {} boot files",
        plan.edition, plan.files, plan.links, plan.stream_bytes, plan.boot_files
    );
    let mut target = FileTarget::create(out, bytes, 512).map_err(err)?;
    let written =
        to_go::write(&mut target, &plan, &mut reader, &wim, &name, &mut Silent).map_err(err)?;
    println!("written in {:.1} s", start.elapsed().as_secs_f64());
    to_go::verify(&mut target, &plan, &written, &mut Silent).map_err(err)?;
    println!(
        "checked the GPT, the boot files and each stream in {:.1} s",
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Something that reads and seeks, for the two kinds of input.
trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}
