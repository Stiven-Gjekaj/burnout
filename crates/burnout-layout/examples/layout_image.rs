//! Build an image of the Windows layout from a directory, so a host can
//! mount it and check it with its own tools.
//!
//! ```text
//! layout_image tree <dir>
//! layout_image image <tree> <image> <manifest> [--install-manifest <file>]
//!     [--large-file <bytes>] [--sector-size 512|4096] [--vhd]
//! ```
//!
//! `tree` writes a sample tree into a new directory. Its bytes come from a
//! generator, so the tree is the same on each host.
//!
//! `image` writes the partition table, formats partition 1 as FAT32 and
//! copies the tree onto it, and writes partition 2 as exFAT with the tree on
//! it. It checks each file of both through a new mount. It writes one line
//! for each file of partition 1 into `<manifest>`, and one line for each file
//! of partition 2 into the file after `--install-manifest`, in the form of
//! `sha256sum`, so a host can check the files it reads with `sha256sum -c`.
//! The example writes the files itself, because a shell that takes the output
//! can change the encoding of a name that is not ASCII. It prints the SHA-256
//! of the image, so two hosts can compare the images they made.
//!
//! `--large-file` adds `sources/install.wim` of that many bytes to partition
//! 2, and makes the image larger by as much. Its bytes come from a generator
//! too, so the host needs no file of that size.
//!
//! `--vhd` adds the footer of a fixed VHD after the image. Windows then mounts
//! the file with `Mount-DiskImage`. The footer is for this check only, and
//! the product never writes it.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use burnout_core::{BlockTarget, FileTarget, Sha256, Window};
use burnout_layout::{
    copy_to_fat32, format_fat32, plan, verify_exfat, verify_fat32, write_exfat, write_table,
    DirSource, Entry, ExfatOptions, Extent, Fat32Options, FileSource, Manifest, TreePath,
};

mod vhd;
use vhd::vhd_footer;

const MIB: u64 = 1024 * 1024;

/// The size of the image with no large file. Partition 1 is large enough for
/// FAT32 on a drive of 4096-byte sectors, which needs about 257 MiB, and
/// partition 2 takes the rest.
const DRIVE_BYTES: u64 = 512 * MIB;
const BOOT_BYTES: u64 = 300 * MIB;

/// Fixed numbers, so the same tree gives the same image on each host.
const DISK_SIGNATURE: u32 = 0x4255_524E;
const SERIAL: u32 = 0x0B0B_0B0B;
const INSTALL_SERIAL: u32 = 0x0E0E_0E0E;

/// Where the large file goes on partition 2, beside the rest of the tree.
const LARGE_FILE: &str = "sources/install.wim";

const USAGE: &str = "usage: layout_image tree <dir>
       layout_image image <tree> <image> <manifest> [--install-manifest <file>]
           [--large-file <bytes>] [--sector-size 512|4096] [--vhd]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("tree") if args.len() == 2 => make_tree(Path::new(&args[1])),
        Some("image") if args.len() >= 4 => match options(&args[4..]) {
            Some(options) => make_image(
                Path::new(&args[1]),
                Path::new(&args[2]),
                Path::new(&args[3]),
                &options,
            ),
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

/// What the flags after the three paths ask for.
struct Options {
    sector: u32,
    vhd: bool,
    install_manifest: Option<PathBuf>,
    large_file: u64,
}

fn options(args: &[String]) -> Option<Options> {
    let mut options = Options {
        sector: 512,
        vhd: false,
        install_manifest: None,
        large_file: 0,
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--sector-size" => options.sector = rest.next()?.parse().ok()?,
            "--vhd" => options.vhd = true,
            "--install-manifest" => options.install_manifest = Some(rest.next()?.into()),
            "--large-file" => options.large_file = rest.next()?.parse().ok()?,
            _ => return None,
        }
    }
    Some(options)
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

fn make_image(tree: &Path, image: &Path, manifest: &Path, options: &Options) -> Outcome {
    let sector = options.sector;
    let source = DirSource::new(tree);
    let drive_bytes = DRIVE_BYTES + options.large_file.div_ceil(MIB) * MIB;
    let mut drive = FileTarget::create(image, drive_bytes, sector)?;
    let layout = plan(drive_bytes, sector, BOOT_BYTES)?;
    write_table(&mut drive, &layout, DISK_SIGNATURE)?;

    let fat32 = Fat32Options {
        label: "Burnout",
        serial: SERIAL,
        first_sector: layout.first_sector(layout.boot) as u32,
    };
    format_fat32(window(&mut drive, layout.boot)?, &fat32)?;
    let boot_files = copy_to_fat32(window(&mut drive, layout.boot)?, &source)?;
    verify_fat32(window(&mut drive, layout.boot)?, &boot_files)?;

    let install_source = WithLargeFile {
        tree: &source,
        path: TreePath::new(LARGE_FILE)?,
        bytes: options.large_file,
    };
    let exfat = ExfatOptions {
        label: "Install",
        serial: INSTALL_SERIAL,
        first_sector: layout.first_sector(layout.install),
    };
    let install_files = write_exfat(window(&mut drive, layout.install)?, &install_source, &exfat)?;
    verify_exfat(window(&mut drive, layout.install)?, &install_files)?;
    drive.sync()?;

    write_manifest(manifest, &boot_files)?;
    if let Some(path) = &options.install_manifest {
        write_manifest(path, &install_files)?;
    }
    println!("{}", digest_of(&mut drive)?);
    drop(drive);

    if options.vhd {
        if sector != 512 {
            return Err("a VHD holds 512-byte sectors only".into());
        }
        let mut file = OpenOptions::new().append(true).open(image)?;
        file.write_all(&vhd_footer(drive_bytes))?;
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

/// One partition of the drive.
fn window(drive: &mut FileTarget, extent: Extent) -> burnout_core::Result<Window<&mut FileTarget>> {
    Window::new(drive, extent.start, extent.length)
}

/// The tree, and a large file of generated bytes beside it, which no host
/// directory has to hold.
struct WithLargeFile<'a> {
    tree: &'a DirSource,
    path: TreePath,
    bytes: u64,
}

impl FileSource for WithLargeFile<'_> {
    fn entries(&self) -> burnout_core::Result<Vec<Entry>> {
        let mut entries = self.tree.entries()?;
        if self.bytes > 0 {
            entries.push(Entry::File(self.path.clone(), self.bytes));
            entries.sort_by(|a, b| a.path().cmp(b.path()));
        }
        Ok(entries)
    }

    fn open(&self, path: &TreePath) -> burnout_core::Result<Box<dyn Read + '_>> {
        if self.bytes > 0 && *path == self.path {
            return Ok(Box::new(Generator::new(self.bytes)));
        }
        self.tree.open(path)
    }
}

/// Bytes from a generator, eight at a time, the same on each host.
struct Generator {
    x: u64,
    word: [u8; 8],
    used: usize,
    left: u64,
}

impl Generator {
    fn new(bytes: u64) -> Self {
        Generator {
            x: 0x9E37_79B9_7F4A_7C15,
            word: [0; 8],
            used: 8,
            left: bytes,
        }
    }
}

impl Read for Generator {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = (buf.len() as u64).min(self.left) as usize;
        for byte in &mut buf[..n] {
            if self.used == 8 {
                self.x ^= self.x << 13;
                self.x ^= self.x >> 7;
                self.x ^= self.x << 17;
                self.word = self.x.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes();
                self.used = 0;
            }
            *byte = self.word[self.used];
            self.used += 1;
        }
        self.left -= n as u64;
        Ok(n)
    }
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
