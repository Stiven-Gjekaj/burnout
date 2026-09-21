//! Build an image of the Windows layout from a directory, so a host can
//! mount it and check it with its own tools.
//!
//! ```text
//! layout_image tree <dir>
//! layout_image image <tree> <image> [--sector-size 512|4096] [--vhd]
//! ```
//!
//! `tree` writes a sample tree into a new directory. Its bytes come from a
//! generator, so the tree is the same on each host.
//!
//! `image` writes the partition table, formats partition 1 as FAT32, copies
//! the tree onto it, and checks each file through a new mount. It prints one
//! line for each file in the form of `sha256sum`, so a host can check the
//! files it reads with `sha256sum -c`. It prints the SHA-256 of the image to
//! standard error, so two hosts can compare the images they made.
//!
//! `--vhd` adds the footer of a fixed VHD after the image. Windows then mounts
//! the file with `Mount-DiskImage`. The footer is for this check only, and
//! the product never writes it.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::ExitCode;

use burnout_core::{BlockTarget, FileTarget, Sha256, Window};
use burnout_layout::{
    copy_to_fat32, format_fat32, plan, verify_fat32, write_table, DirSource, Fat32Options, Layout,
};

const MIB: u64 = 1024 * 1024;

/// The size of the image. Partition 1 is large enough for FAT32 on a drive
/// of 4096-byte sectors, which needs about 257 MiB.
const DRIVE_BYTES: u64 = 512 * MIB;
const BOOT_BYTES: u64 = 300 * MIB;

/// Fixed numbers, so the same tree gives the same image on each host.
const DISK_SIGNATURE: u32 = 0x4255_524E;
const SERIAL: u32 = 0x0B0B_0B0B;

const USAGE: &str = "usage: layout_image tree <dir>
       layout_image image <tree> <image> [--sector-size 512|4096] [--vhd]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("tree") if args.len() == 2 => make_tree(Path::new(&args[1])),
        Some("image") if args.len() >= 3 => match options(&args[3..]) {
            Some((sector, vhd)) => {
                make_image(Path::new(&args[1]), Path::new(&args[2]), sector, vhd)
            }
            None => return usage(),
        },
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("layout_image: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

/// The sector size and whether to add a VHD footer.
fn options(args: &[String]) -> Option<(u32, bool)> {
    let mut sector = 512;
    let mut vhd = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--sector-size" => sector = rest.next()?.parse().ok()?,
            "--vhd" => vhd = true,
            _ => return None,
        }
    }
    Some((sector, vhd))
}

type Outcome = Result<(), Box<dyn std::error::Error>>;

/// Write the sample tree: files at several depths, a file of no bytes, long
/// names, a name that is not ASCII, an empty directory, and a directory that
/// holds more entries than one cluster.
fn make_tree(root: &Path) -> Outcome {
    fs::create_dir(root)?;
    let files: &[(&str, usize)] = &[
        ("bootmgr", 4000),
        ("efi/boot/bootx64.efi", 12_289),
        ("efi/microsoft/boot/BCD", 3 * 4096),
        ("sources/boot.wim", 5 * MIB as usize + 7),
        ("autorun.inf", 0),
        ("A long name with spaces.txt", 10),
        ("\u{dc}berpr\u{fc}fung.txt", 100),
    ];
    for (seed, (path, length)) in files.iter().enumerate() {
        write_file(&root.join(path), &generated(*length, seed as u64))?;
    }
    for n in 0..200 {
        let path = root.join(format!("many/file number {n:03}.txt"));
        write_file(&path, &generated(n * 37, 1000 + n as u64))?;
    }
    fs::create_dir_all(root.join("support/logging"))?;
    Ok(())
}

fn write_file(path: &Path, bytes: &[u8]) -> Outcome {
    fs::create_dir_all(path.parent().expect("a file has a directory"))?;
    fs::write(path, bytes)?;
    Ok(())
}

/// Bytes from a generator, the same on each host.
fn generated(length: usize, seed: u64) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ seed;
    (0..length)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn make_image(tree: &Path, image: &Path, sector: u32, vhd: bool) -> Outcome {
    let source = DirSource::new(tree);
    let mut drive = FileTarget::create(image, DRIVE_BYTES, sector)?;
    let layout = plan(DRIVE_BYTES, sector, BOOT_BYTES)?;
    write_table(&mut drive, &layout, DISK_SIGNATURE)?;

    let options = Fat32Options {
        label: "Burnout",
        serial: SERIAL,
        first_sector: layout.first_sector(layout.boot) as u32,
    };
    format_fat32(boot(&mut drive, &layout)?, &options)?;
    let manifest = copy_to_fat32(boot(&mut drive, &layout)?, &source)?;
    verify_fat32(boot(&mut drive, &layout)?, &manifest)?;
    drive.sync()?;

    for file in &manifest.files {
        println!("{}  {}", file.digest, file.path);
    }
    eprintln!("image: {}", digest_of(&mut drive)?);
    drop(drive);

    if vhd {
        if sector != 512 {
            return Err("a VHD holds 512-byte sectors only".into());
        }
        let mut file = OpenOptions::new().append(true).open(image)?;
        file.write_all(&vhd_footer(DRIVE_BYTES))?;
        file.sync_all()?;
    }
    Ok(())
}

/// Partition 1 of the drive.
fn boot<'a>(
    drive: &'a mut FileTarget,
    layout: &Layout,
) -> burnout_core::Result<Window<&'a mut FileTarget>> {
    Window::new(drive, layout.boot.start, layout.boot.length)
}

/// The SHA-256 of the whole image.
fn digest_of(drive: &mut FileTarget) -> Result<String, std::io::Error> {
    let mut hash = Sha256::new();
    let mut block = vec![0u8; 4 * MIB as usize];
    drive.seek(SeekFrom::Start(0))?;
    loop {
        match drive.read(&mut block)? {
            0 => return Ok(hash.finish().to_string()),
            n => hash.update(&block[..n]),
        }
    }
}

/// The footer of a fixed VHD for a disk of `size` bytes, as the VHD
/// specification gives it.
fn vhd_footer(size: u64) -> [u8; 512] {
    let mut footer = [0u8; 512];
    footer[0..8].copy_from_slice(b"conectix");
    footer[8..12].copy_from_slice(&2u32.to_be_bytes());
    footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    // A fixed disk has no header, so the offset of the header is all ones.
    footer[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
    // The time stays zero, so two runs give one file.
    footer[28..32].copy_from_slice(b"brnt");
    footer[32..36].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    footer[36..40].copy_from_slice(b"Wi2k");
    footer[40..48].copy_from_slice(&size.to_be_bytes());
    footer[48..56].copy_from_slice(&size.to_be_bytes());
    let (cylinders, heads, sectors) = vhd_geometry(size / 512);
    footer[56..58].copy_from_slice(&cylinders.to_be_bytes());
    footer[58] = heads;
    footer[59] = sectors;
    footer[60..64].copy_from_slice(&2u32.to_be_bytes());
    footer[68..84].copy_from_slice(b"burnout-layout-1");
    let sum: u32 = footer.iter().map(|b| *b as u32).sum();
    footer[64..68].copy_from_slice(&(!sum).to_be_bytes());
    footer
}

/// The geometry that the VHD specification computes from a sector count.
fn vhd_geometry(sectors: u64) -> (u16, u8, u8) {
    let total = sectors.min(65_535 * 16 * 255);
    let (per_track, heads, cylinder_times_heads) = if total >= 65_535 * 16 * 63 {
        (255, 16, total / 255)
    } else {
        let mut per_track = 17;
        let mut cylinder_times_heads = total / per_track;
        let mut heads = cylinder_times_heads.div_ceil(1024).max(4);
        if cylinder_times_heads >= heads * 1024 || heads > 16 {
            per_track = 31;
            heads = 16;
            cylinder_times_heads = total / per_track;
        }
        if cylinder_times_heads >= heads * 1024 {
            per_track = 63;
            heads = 16;
            cylinder_times_heads = total / per_track;
        }
        (per_track, heads, cylinder_times_heads)
    };
    (
        (cylinder_times_heads / heads) as u16,
        heads as u8,
        per_track as u8,
    )
}
