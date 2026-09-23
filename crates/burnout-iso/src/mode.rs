//! The mode that an image asks for: a copy byte for byte, or the layout of
//! a drive that Windows starts from. macOS media is refused.
//!
//! The rules, in order:
//!
//! - An installer package, a compressed disk image of macOS, an
//!   application bundle, or an ISO that holds one, is macOS media.
//! - A boot table in the first sector is raw mode. The image is a hybrid,
//!   which starts from a disc and from a drive.
//! - An ISO with `sources/install.wim` or `sources/install.esd` is Windows
//!   mode.
//! - Anything else is raw mode, and the drive may start nothing.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use burnout_core::{has_boot_table, Entry, Error, FileSource, Result, BOOT_SECTOR_BYTES};

use crate::IsoSource;

/// What Burnout does with an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A copy of the image, byte for byte. A drive starts from the copy
    /// when the first sector of the image holds a boot table.
    Raw { boot_table: bool },
    /// A drive laid out for Windows, with the files of the image on it.
    Windows,
}

/// The mode for the image at `path`.
pub fn mode_of_file(path: &Path) -> Result<Mode> {
    let name = path.display().to_string();
    let unreadable = |e: std::io::Error| Error::Image {
        path: name.clone(),
        detail: e.to_string(),
    };
    if fs::metadata(path).map_err(unreadable)?.is_dir() {
        let app = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("app"));
        return Err(match app {
            true => macos(&name, "an application bundle"),
            false => Error::Image {
                path: name,
                detail: "it is a directory, and not an image".to_string(),
            },
        });
    }
    let file = File::open(path).map_err(unreadable)?;
    mode_of(file, &name)
}

/// The mode for an image, from its bytes. `name` names it in each error.
pub fn mode_of<R: Read + Seek>(mut reader: R, name: &str) -> Result<Mode> {
    let length = reader.seek(SeekFrom::End(0))?;
    let first = read_at(&mut reader, 0, length.min(BOOT_SECTOR_BYTES as u64))?;
    if first.starts_with(b"xar!") {
        return Err(macos(name, "an installer package"));
    }
    if let Some(at) = length.checked_sub(512) {
        if is_compressed_dmg(&read_at(&mut reader, at, 512)?) {
            return Err(macos(name, "a compressed disk image of macOS"));
        }
    }
    if has_boot_table(&first) {
        return Ok(Mode::Raw { boot_table: true });
    }
    let Some(source) = IsoSource::new(reader, name)? else {
        return Ok(Mode::Raw { boot_table: false });
    };
    let entries = source.entries()?;
    let installer = entries.iter().any(|e| match e {
        Entry::Dir(p) => p.parent().is_none() && p.name().to_ascii_lowercase().ends_with(".app"),
        Entry::File(..) => false,
    });
    if installer {
        return Err(macos(name, "an image of a macOS installer"));
    }
    let windows = entries.iter().any(|e| match e {
        Entry::File(p, _) => {
            let p = p.as_str().to_ascii_lowercase();
            p == "sources/install.wim" || p == "sources/install.esd"
        }
        Entry::Dir(_) => false,
    });
    Ok(match windows {
        true => Mode::Windows,
        false => Mode::Raw { boot_table: false },
    })
}

fn macos(name: &str, what: &'static str) -> Error {
    Error::MacosMedia {
        path: name.to_string(),
        what,
    }
}

fn read_at<R: Read + Seek>(reader: &mut R, at: u64, length: u64) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; length as usize];
    reader.seek(SeekFrom::Start(at))?;
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Whether the last 512 bytes are the trailer of a UDIF disk image whose
/// data holds fewer bytes than the sectors that it describes.
///
/// `hdiutil` gives each compressed and read-only format this trailer, and
/// its raw format no trailer. A byte copy of a compressed image is not a
/// drive.
fn is_compressed_dmg(trailer: &[u8]) -> bool {
    if !trailer.starts_with(b"koly") {
        return false;
    }
    let big = |at: usize| u64::from_be_bytes(trailer[at..at + 8].try_into().unwrap());
    let (data_fork, sectors) = (big(0x20), big(0x1EC));
    data_fork < sectors.saturating_mul(512)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{bridge, iso9660, udf, Iso, Item, Udf};
    use std::io::Cursor;

    fn mode(image: Vec<u8>) -> Result<Mode> {
        mode_of(Cursor::new(image), "test.img")
    }

    fn refused(image: Vec<u8>) -> String {
        let e = mode(image).unwrap_err();
        assert!(matches!(e, Error::MacosMedia { .. }), "{e}");
        e.to_string()
    }

    /// The trailer of a UDIF image, after `data` bytes of data.
    fn with_trailer(data: usize, data_fork: u64, sectors: u64) -> Vec<u8> {
        let mut image = vec![0x11u8; data];
        let mut koly = vec![0u8; 512];
        koly[..4].copy_from_slice(b"koly");
        koly[0x20..0x28].copy_from_slice(&data_fork.to_be_bytes());
        koly[0x1EC..0x1F4].copy_from_slice(&sectors.to_be_bytes());
        image.extend(koly);
        image
    }

    const NOTE: &[Item] = &[Item::File("README.TXT", b"use a reader of UDF")];

    #[test]
    fn a_hybrid_image_is_raw_mode_with_its_boot_table() {
        let mut image = iso9660(&[Item::File("a.txt", b"a")], Iso::default());
        image[510] = 0x55;
        image[511] = 0xAA;
        image[446 + 4] = 0xEE;
        assert_eq!(mode(image).unwrap(), Mode::Raw { boot_table: true });
    }

    #[test]
    fn an_iso_with_an_install_image_is_windows_mode() {
        for path in [
            "sources/install.wim",
            "sources/install.esd",
            "Sources/Install.WIM",
        ] {
            let dir = path.split('/').next().unwrap();
            let items = [Item::Dir(dir), Item::File(path, b"MSWIM")];
            assert_eq!(mode(bridge(NOTE, &items)).unwrap(), Mode::Windows, "{path}");
        }
        let items = [
            Item::Dir("sources"),
            Item::File("sources/install.wim", b"x"),
        ];
        let joliet = Iso {
            joliet: true,
            ..Iso::default()
        };
        assert_eq!(mode(iso9660(&items, joliet)).unwrap(), Mode::Windows);
    }

    #[test]
    fn an_install_image_in_another_directory_is_not_windows_mode() {
        let items = [Item::Dir("x"), Item::File("x/install.wim", b"x")];
        let found = mode(udf(&items, &Udf::default())).unwrap();
        assert_eq!(found, Mode::Raw { boot_table: false });
    }

    #[test]
    fn an_image_that_is_no_iso_is_raw_mode_without_a_boot_table() {
        for image in [vec![0u8; 400 * 2048], vec![7u8; 100], Vec::new()] {
            assert_eq!(mode(image).unwrap(), Mode::Raw { boot_table: false });
        }
    }

    #[test]
    fn an_installer_package_is_refused() {
        let mut image = b"xar!".to_vec();
        image.resize(4096, 0);
        let e = refused(image);
        assert!(e.contains("an installer package"), "{e}");
        assert!(e.contains("createinstallmedia"), "{e}");
    }

    #[test]
    fn a_compressed_dmg_is_refused_and_a_complete_one_is_not() {
        let e = refused(with_trailer(4096, 4096, 100));
        assert!(e.contains("a compressed disk image of macOS"), "{e}");
        let complete = with_trailer(4096, 4096, 8);
        assert_eq!(mode(complete).unwrap(), Mode::Raw { boot_table: false });
    }

    #[test]
    fn an_iso_that_holds_a_macos_installer_is_refused() {
        let items = [
            Item::Dir("Install macOS Sequoia.app"),
            Item::Dir("Install macOS Sequoia.app/Contents"),
            Item::File("Install macOS Sequoia.app/Contents/Info.plist", b"<plist/>"),
        ];
        let e = refused(udf(&items, &Udf::default()));
        assert!(e.contains("an image of a macOS installer"), "{e}");
    }

    #[test]
    fn a_directory_is_refused_and_an_app_bundle_names_createinstallmedia() {
        let base = std::env::temp_dir().join(format!("burnout-mode-{}", std::process::id()));
        let (app, plain) = (base.join("Install macOS.app"), base.join("folder"));
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&plain).unwrap();
        let app_error = mode_of_file(&app).unwrap_err();
        let plain_error = mode_of_file(&plain).unwrap_err();
        fs::remove_dir_all(&base).unwrap();
        assert!(matches!(app_error, Error::MacosMedia { .. }), "{app_error}");
        assert!(
            plain_error.to_string().contains("it is a directory"),
            "{plain_error}"
        );
    }
}
