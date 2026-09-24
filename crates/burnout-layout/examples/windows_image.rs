//! Write the drive of Windows mode from an ISO into an image file, with the
//! code that `burnout write` runs on a drive.
//!
//! ```text
//! windows_image <iso> <image> [--size <bytes>] [--sector-size 512|4096]
//!     [--skip-hardware-checks] [--no-microsoft-account] [--edition <index>]
//!     [--boot-manifest <file>] [--install-manifest <file>] [--vhd]
//! ```
//!
//! The image is the smallest drive that holds the layout, or `--size` bytes.
//! The example writes the table, both volumes and every file, and then checks
//! the image through a new handle, as the product checks a drive.
//!
//! The manifests give one line for each file of partition 1 and of partition
//! 2, in the form of `sha256sum`, so a host can mount the image and check
//! each file with its own tools. `--vhd` adds the footer of a fixed VHD, so
//! Windows can mount the image.
//!
//! The serials are fixed, so one ISO gives the same image on each host.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use burnout_core::{FileTarget, ProgressEvent, Stage};
use burnout_iso::IsoSource;
use burnout_layout::{
    unattend_xml, verify_windows, write_windows, Manifest, Serials, Unattend, WindowsDrive,
    WindowsTree,
};

mod vhd;
use vhd::vhd_footer;

const USAGE: &str = "usage: windows_image <iso> <image> [--size <bytes>] [--sector-size 512|4096]
           [--skip-hardware-checks] [--no-microsoft-account] [--edition <index>]
           [--boot-manifest <file>] [--install-manifest <file>] [--vhd]";

const SERIALS: Serials = Serials {
    disk: 0x4255_524E,
    boot: 0x0B0B_0B0B,
    install: 0x0E0E_0E0E,
};

#[derive(Default)]
struct Options {
    size: Option<u64>,
    sector: u32,
    skip_hardware_checks: bool,
    no_microsoft_account: bool,
    edition: Option<u32>,
    boot_manifest: Option<PathBuf>,
    install_manifest: Option<PathBuf>,
    vhd: bool,
}

fn options(args: &[String]) -> Option<Options> {
    let mut o = Options {
        sector: 512,
        ..Options::default()
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--size" => o.size = Some(rest.next()?.parse().ok()?),
            "--sector-size" => o.sector = rest.next()?.parse().ok()?,
            "--skip-hardware-checks" => o.skip_hardware_checks = true,
            "--no-microsoft-account" => o.no_microsoft_account = true,
            "--edition" => o.edition = Some(rest.next()?.parse().ok()?),
            "--boot-manifest" => o.boot_manifest = Some(rest.next()?.into()),
            "--install-manifest" => o.install_manifest = Some(rest.next()?.into()),
            "--vhd" => o.vhd = true,
            _ => return None,
        }
    }
    Some(o)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(o) = (args.len() >= 2).then(|| options(&args[2..])).flatten() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match run(&args[0], Path::new(&args[1]), &o) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("windows_image: {e}");
            ExitCode::FAILURE
        }
    }
}

type Outcome = Result<(), Box<dyn std::error::Error>>;

fn run(iso: &str, image: &Path, o: &Options) -> Outcome {
    let file = File::open(iso).map_err(|e| format!("cannot open {iso}: {e}"))?;
    let source = IsoSource::new(file, iso)?
        .ok_or_else(|| format!("{iso} holds neither UDF nor ISO 9660"))?;
    let tree = WindowsTree::new(&source)?;
    let unattend = unattend_xml(&Unattend {
        architecture: tree.architecture,
        image: tree.image,
        edition: o.edition,
        skip_hardware_checks: o.skip_hardware_checks,
        no_microsoft_account: o.no_microsoft_account,
    });
    let bytes = match o.size {
        Some(bytes) => bytes,
        None => tree.needed_drive_bytes(o.sector)?,
    };
    let layout = tree.plan(bytes, o.sector)?;
    let plan = WindowsDrive {
        layout,
        label: source.label(),
        serials: SERIALS,
        unattend: unattend.as_bytes(),
    };
    let (files, boot_bytes) = tree.boot_files();
    println!(
        "{} ({}), label {}",
        iso,
        source.file_system(),
        source.label()
    );
    println!(
        "partition 1: FAT32, {} MiB, {files} boot files of {boot_bytes} bytes and {}",
        layout.boot.length >> 20,
        burnout_layout::UNATTEND_FILE
    );
    println!(
        "partition 2: exFAT, {} MiB, {} of {} bytes",
        layout.install.length >> 20,
        tree.image_path(),
        tree.image_bytes()
    );

    let mut progress = |e: ProgressEvent| {
        if let ProgressEvent::Done { stage, bytes_done } = e {
            if stage != Stage::Flush {
                println!("{stage:?}: {bytes_done} bytes");
            }
        }
    };
    let written = {
        let mut drive = FileTarget::create(image, bytes, o.sector)?;
        write_windows(&mut drive, &tree, &source, &plan, &mut progress)?
    };
    let mut again = FileTarget::new(
        OpenOptions::new().read(true).write(true).open(image)?,
        o.sector,
    )?;
    verify_windows(&mut again, &plan, &written, &mut progress)?;
    let (n, total) = written.files();
    println!("checked {n} files of {total} bytes, each against the SHA-256 that it went in with");

    if let Some(path) = &o.boot_manifest {
        write_manifest(path, &written.boot)?;
    }
    if let Some(path) = &o.install_manifest {
        write_manifest(path, &written.install)?;
    }
    if o.vhd {
        if o.sector != 512 {
            return Err("a VHD holds 512-byte sectors only".into());
        }
        let mut file = OpenOptions::new().append(true).open(image)?;
        file.write_all(&vhd_footer(bytes))?;
        file.sync_all()?;
    }
    Ok(())
}

/// One line for each file, in the form of `sha256sum`.
fn write_manifest(path: &Path, copied: &Manifest) -> Outcome {
    let mut lines = String::new();
    for file in &copied.files {
        lines.push_str(&format!("{}  {}\n", file.digest, file.path));
    }
    fs::write(path, lines)?;
    Ok(())
}
