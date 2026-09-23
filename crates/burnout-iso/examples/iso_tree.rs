//! Read an ISO with the reader of Burnout, so a person or a host can check
//! what it gives.
//!
//! ```text
//! iso_tree list <image>
//! iso_tree extract <image> <path> <out>
//! iso_tree manifest <image> <out>
//! iso_tree compare <image> <dir>
//! iso_tree mode <image>
//! ```
//!
//! `list` gives the file system and the label, then one line for each
//! directory (`d path`), file (`f path bytes`) and link (`l path -> target`).
//!
//! `extract` copies one file of the image into `<out>`, and gives its
//! SHA-256.
//!
//! `manifest` writes one line for each file into `<out>`, in the form of
//! `sha256sum`, so a host can check the files that its own mount gives with
//! `sha256sum -c`. The example writes the file itself, because a shell that
//! takes the output can change the encoding of a name that is not ASCII.
//!
//! `compare` reads the image and a directory of the host, and fails unless
//! both hold the same directories, the same files and the same bytes. The
//! gate in CI uses it on an ISO that the tools of each host make from the
//! sample tree.
//!
//! `mode` gives the mode that the image asks for, `neither` when no mode
//! fits it, or the refusal.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;

use burnout_core::{DirSource, Entry, FileSource, Sha256, TreePath};
use burnout_iso::{mode_of_file, IsoSource, Mode};

const USAGE: &str = "usage: iso_tree list <image>
       iso_tree extract <image> <path> <out>
       iso_tree manifest <image> <out>
       iso_tree compare <image> <dir>
       iso_tree mode <image>";

type Outcome = Result<(), Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match (args.first().map(String::as_str), args.len()) {
        (Some("list"), 2) => list(&args[1]),
        (Some("extract"), 4) => extract(&args[1], &args[2], Path::new(&args[3])),
        (Some("manifest"), 3) => manifest(&args[1], Path::new(&args[2])),
        (Some("compare"), 3) => compare(&args[1], Path::new(&args[2])),
        (Some("mode"), 2) => mode(&args[1]),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        // A reader such as `head` that stops early is not a fault.
        Err(e)
            if e.downcast_ref::<std::io::Error>().map(std::io::Error::kind)
                == Some(std::io::ErrorKind::BrokenPipe) =>
        {
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("iso_tree: {e}");
            ExitCode::FAILURE
        }
    }
}

fn open(image: &str) -> Result<IsoSource<File>, Box<dyn std::error::Error>> {
    let file = File::open(image).map_err(|e| format!("cannot open {image}: {e}"))?;
    IsoSource::new(file, image)?
        .ok_or_else(|| format!("{image} holds neither UDF nor ISO 9660").into())
}

fn list(image: &str) -> Outcome {
    let source = open(image)?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "# {}", source.file_system())?;
    writeln!(out, "# label {}", source.label())?;
    for entry in source.entries()? {
        match entry {
            Entry::Dir(path) => writeln!(out, "d {path}")?,
            Entry::File(path, bytes) => writeln!(out, "f {path} {bytes}")?,
        }
    }
    for link in source.links() {
        writeln!(out, "l {} -> {}", link.path, link.target)?;
    }
    Ok(())
}

/// Read one file of a tree to its end, and give its SHA-256. `out` takes
/// the bytes too, when it is there.
fn digest(
    source: &dyn FileSource,
    path: &TreePath,
    mut out: Option<&mut File>,
) -> Result<(String, u64), Box<dyn std::error::Error>> {
    let mut file = source.open(path)?;
    let mut hash = Sha256::new();
    let mut block = vec![0u8; 4 << 20];
    let mut bytes = 0u64;
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            return Ok((hash.finish().to_string(), bytes));
        }
        hash.update(&block[..n]);
        if let Some(out) = out.as_deref_mut() {
            out.write_all(&block[..n])?;
        }
        bytes += n as u64;
    }
}

fn extract(image: &str, path: &str, out: &Path) -> Outcome {
    let source = open(image)?;
    let mut file = File::create(out)?;
    let (digest, _) = digest(&source, &TreePath::new(path)?, Some(&mut file))?;
    println!("{digest}  {path}");
    Ok(())
}

fn manifest(image: &str, out: &Path) -> Outcome {
    let source = open(image)?;
    let mut lines = String::new();
    for entry in source.entries()? {
        if let Entry::File(path, _) = entry {
            let (digest, _) = digest(&source, &path, None)?;
            lines.push_str(&format!("{digest}  {path}\n"));
        }
    }
    fs::write(out, lines)?;
    Ok(())
}

fn compare(image: &str, dir: &Path) -> Outcome {
    let source = open(image)?;
    let tree = DirSource::new(dir);
    let (ours, theirs) = (source.entries()?, tree.entries()?);
    let mut faults = Vec::new();
    for link in source.links() {
        faults.push(format!("the image holds a link at {}", link.path));
    }
    let by_path = |entries: &[Entry]| -> BTreeMap<TreePath, Entry> {
        entries
            .iter()
            .map(|e| (e.path().clone(), e.clone()))
            .collect()
    };
    let (ours_by_path, theirs_by_path) = (by_path(&ours), by_path(&theirs));
    for (path, entry) in &theirs_by_path {
        match ours_by_path.get(path) {
            None => faults.push(format!("the image does not hold {path}")),
            Some(found) if found != entry => faults.push(format!(
                "{path} is {found:?} in the image and {entry:?} in the tree"
            )),
            Some(Entry::File(..)) => {
                let (a, _) = digest(&source, path, None)?;
                let (b, _) = digest(&tree, path, None)?;
                if a != b {
                    faults.push(format!("{path} has other bytes in the image"));
                }
            }
            Some(Entry::Dir(_)) => {}
        }
    }
    for path in ours_by_path.keys() {
        if !theirs_by_path.contains_key(path) {
            faults.push(format!("the image holds {path}, and the tree does not"));
        }
    }
    let files = theirs
        .iter()
        .filter(|e| matches!(e, Entry::File(..)))
        .count();
    let dirs = theirs.len() - files;
    if faults.is_empty() {
        println!(
            "{} ({}) holds the {dirs} directories and the {files} files of {}, byte for byte",
            image,
            source.file_system(),
            dir.display()
        );
        return Ok(());
    }
    for fault in &faults {
        eprintln!("{fault}");
    }
    Err(format!("{} faults", faults.len()).into())
}

fn mode(image: &str) -> Outcome {
    match mode_of_file(Path::new(image))? {
        Mode::Raw => println!("raw"),
        Mode::Windows => println!("windows"),
        Mode::Neither => println!("neither: no boot table, and no install image of Windows"),
    }
    Ok(())
}
