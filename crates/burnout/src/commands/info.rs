//! The info command: what an image is, and what Burnout would do with it.

use std::fs::File;
use std::path::Path;

use burnout_core::{human_size, size_column, Entry, Error, FileSource, Result};
use burnout_iso::{check_whole, mode_of_file, IsoSource, Mode};
use burnout_wim::{read_wim, Edition, Wim};

use crate::json::Json;

/// What `info` found in an image.
#[derive(Debug)]
struct Info {
    path: String,
    bytes: u64,
    mode: Mode,
    /// The file system of the tree and its label, for an ISO.
    volume: Option<(String, String)>,
    install: Option<Install>,
}

/// The install image of a Windows ISO.
#[derive(Debug)]
struct Install {
    path: String,
    bytes: u64,
    wim: Wim,
}

/// Print what the image at `image` is, and the mode that `write` would use.
/// It reads the image only, and needs no privilege.
pub fn run(image: &Path, json: bool) -> Result<i32> {
    let info = read(image)?;
    if json {
        println!("{}", info_json(&info));
    } else {
        for line in info_lines(&info) {
            println!("{line}");
        }
    }
    Ok(0)
}

fn read(image: &Path) -> Result<Info> {
    let path = image.display().to_string();
    let unreadable = |e: std::io::Error| Error::Image {
        path: path.clone(),
        detail: e.to_string(),
    };
    // The mode check names a directory and a missing file, so it comes
    // first.
    let mode = mode_of_file(image)?;
    let mut file = File::open(image).map_err(unreadable)?;
    let bytes = file.metadata().map_err(unreadable)?.len();
    check_whole(&mut file, &path)?;

    let Some(source) = IsoSource::new(file, &path)? else {
        return Ok(Info {
            path,
            bytes,
            mode,
            volume: None,
            install: None,
        });
    };
    let volume = Some((source.file_system().to_string(), source.label().to_string()));
    let install = match mode {
        Mode::Windows => Some(install_image(&source, &path)?),
        _ => None,
    };
    Ok(Info {
        path,
        bytes,
        mode,
        volume,
        install,
    })
}

fn install_image(source: &IsoSource<File>, iso: &str) -> Result<Install> {
    let found = source.entries()?.into_iter().find_map(|e| match e {
        Entry::File(p, bytes) => {
            let lower = p.as_str().to_ascii_lowercase();
            (lower == "sources/install.wim" || lower == "sources/install.esd").then_some((p, bytes))
        }
        Entry::Dir(_) => None,
    });
    let (tree_path, bytes) = found.ok_or_else(|| Error::Image {
        path: iso.to_string(),
        detail: "it holds no sources/install.wim and no sources/install.esd".to_string(),
    })?;
    let name = format!("{iso}: {}", tree_path.as_str());
    let wim = read_wim(source.open_file(&tree_path)?, &name)?;
    Ok(Install {
        path: tree_path.as_str().to_string(),
        bytes,
        wim,
    })
}

fn mode_word(mode: Mode) -> &'static str {
    match mode {
        Mode::Raw => "raw",
        Mode::Windows => "windows",
        Mode::Neither => "none",
    }
}

fn info_lines(info: &Info) -> Vec<String> {
    let mut lines = vec![
        format!("image   {}", info.path),
        format!("        {}", size_column(info.bytes)),
    ];
    if let Some((file_system, label)) = &info.volume {
        let label = match label.is_empty() {
            true => String::new(),
            false => format!(", label {label}"),
        };
        lines.push(format!("volume  {file_system}{label}"));
    }
    lines.push(format!("mode    {}", mode_word(info.mode)));
    lines.push(String::new());
    lines.push(
        match info.mode {
            Mode::Raw => "The first sector holds a boot table, so `write` copies the image byte for byte.",
            Mode::Windows => "The image holds an install image of Windows, so `write` lays the drive out for Windows.",
            Mode::Neither => "The first sector holds no boot table, and the image holds no install image of Windows. `write` refuses it on a whole drive. Use --mode raw to copy it anyway, or --partition to write it into one partition.",
        }
        .to_string(),
    );
    if let Some(install) = &info.install {
        let header = &install.wim.header;
        lines.push(String::new());
        lines.push(format!(
            "install {}, {}, {} compression",
            install.path,
            human_size(install.bytes),
            header.compression().name()
        ));
        let editions = &install.wim.editions;
        let width = editions
            .iter()
            .map(|e| e.shown_name().chars().count())
            .max()
            .unwrap_or(0);
        for e in editions {
            lines.push(edition_line(e, width));
        }
        lines.push(String::new());
        lines.push(
            "Setup asks for one of these editions, or write --edition chooses one.".to_string(),
        );
    }
    lines
}

fn edition_line(e: &Edition, width: usize) -> String {
    let mut parts = vec![format!("{:>4}  {:<width$}", e.index, e.shown_name())];
    if let Some(arch) = e.arch {
        parts.push(arch.name());
    }
    if !e.version.is_empty() {
        parts.push(e.version.clone());
    }
    if !e.default_language.is_empty() {
        parts.push(e.default_language.clone());
    }
    parts.join("  ").trim_end().to_string()
}

fn info_json(info: &Info) -> Json {
    let volume = match &info.volume {
        Some((file_system, label)) => Json::Object(vec![
            ("file_system", Json::text(file_system)),
            ("label", Json::text(label)),
        ]),
        None => Json::Null,
    };
    let install = match &info.install {
        Some(install) => Json::Object(vec![
            ("path", Json::text(&install.path)),
            ("size_bytes", Json::Number(install.bytes)),
            (
                "compression",
                Json::text(install.wim.header.compression().name()),
            ),
            (
                "editions",
                Json::List(install.wim.editions.iter().map(edition_json).collect()),
            ),
        ]),
        None => Json::Null,
    };
    Json::Object(vec![
        (
            "image",
            Json::Object(vec![
                ("path", Json::text(&info.path)),
                ("size_bytes", Json::Number(info.bytes)),
            ]),
        ),
        ("mode", Json::text(mode_word(info.mode))),
        ("volume", volume),
        ("install_image", install),
    ])
}

fn edition_json(e: &Edition) -> Json {
    let empty_null = |s: &str| Json::maybe_text(Some(s).filter(|s| !s.is_empty()));
    Json::Object(vec![
        ("index", Json::Number(e.index as u64)),
        ("name", Json::text(&e.name)),
        ("display_name", empty_null(&e.display_name)),
        ("edition_id", empty_null(&e.edition_id)),
        (
            "arch",
            Json::maybe_text(e.arch.map(|a| a.name()).as_deref()),
        ),
        ("version", empty_null(&e.version)),
        (
            "languages",
            Json::List(e.languages.iter().map(Json::text).collect()),
        ),
        ("default_language", empty_null(&e.default_language)),
        ("total_bytes", Json::maybe_number(e.total_bytes)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use burnout_wim::{Arch, Header};

    fn header() -> Header {
        // The smallest WIM header that parses, with LZX.
        let mut b = vec![0u8; burnout_wim::HEADER_BYTES];
        b[..8].copy_from_slice(b"MSWIM\0\0\0");
        b[8] = 208;
        b[0x10..0x14].copy_from_slice(&0x0004_0002u32.to_le_bytes());
        b[0x28] = 1;
        b[0x2A] = 1;
        Header::parse(&b, "t").unwrap()
    }

    fn windows() -> Info {
        let pro = Edition {
            index: 1,
            name: "Windows 11 Pro".to_string(),
            edition_id: "Professional".to_string(),
            arch: Some(Arch::X64),
            version: "10.0.26100.1742".to_string(),
            languages: vec!["en-US".to_string()],
            default_language: "en-US".to_string(),
            total_bytes: Some(20_000_000_000),
            ..Edition::default()
        };
        let home = Edition {
            index: 2,
            name: "Home".to_string(),
            display_name: "Windows 11 Home".to_string(),
            ..Edition::default()
        };
        Info {
            path: "Win11.iso".to_string(),
            bytes: 7_994_415_104,
            mode: Mode::Windows,
            volume: Some(("UDF".to_string(), "CCCOMA_A64FRE".to_string())),
            install: Some(Install {
                path: "sources/install.wim".to_string(),
                bytes: 6_000_000_000,
                wim: Wim {
                    header: header(),
                    editions: vec![pro, home],
                },
            }),
        }
    }

    #[test]
    fn a_windows_iso_shows_its_volume_and_its_editions() {
        let lines = info_lines(&windows());
        assert_eq!(lines[0], "image   Win11.iso");
        assert_eq!(lines[1], "        8.0 GB (7,994,415,104 bytes)");
        assert_eq!(lines[2], "volume  UDF, label CCCOMA_A64FRE");
        assert_eq!(lines[3], "mode    windows");
        assert!(lines.contains(&"install sources/install.wim, 6.0 GB, LZX compression".to_string()));
        assert!(lines.contains(&"   1  Windows 11 Pro   x64  10.0.26100.1742  en-US".to_string()));
        assert!(
            lines.contains(&"   2  Windows 11 Home".to_string()),
            "{lines:#?}"
        );
    }

    #[test]
    fn an_image_of_neither_mode_says_what_to_do() {
        let info = Info {
            path: "fs.img".to_string(),
            bytes: 1000,
            mode: Mode::Neither,
            volume: None,
            install: None,
        };
        let lines = info_lines(&info);
        assert_eq!(lines[2], "mode    none");
        assert!(lines[4].contains("--mode raw") && lines[4].contains("--partition"));
        assert!(!lines.iter().any(|l| l.starts_with("volume")));
    }

    #[test]
    fn the_json_holds_the_same_facts() {
        let json = info_json(&windows()).to_string();
        assert!(json.starts_with(
            r#"{"image":{"path":"Win11.iso","size_bytes":7994415104},"mode":"windows","volume":{"file_system":"UDF","label":"CCCOMA_A64FRE"},"install_image":{"path":"sources/install.wim","size_bytes":6000000000,"compression":"LZX","editions":[{"index":1,"name":"Windows 11 Pro","display_name":null,"edition_id":"Professional","arch":"x64","version":"10.0.26100.1742","languages":["en-US"],"default_language":"en-US","total_bytes":20000000000}"#
        ), "{json}");
        assert!(json.contains(r#"{"index":2,"name":"Home","display_name":"Windows 11 Home","edition_id":null,"arch":null"#));

        let raw = Info {
            path: "a.iso".to_string(),
            bytes: 5,
            mode: Mode::Raw,
            volume: None,
            install: None,
        };
        assert_eq!(
            info_json(&raw).to_string(),
            r#"{"image":{"path":"a.iso","size_bytes":5},"mode":"raw","volume":null,"install_image":null}"#
        );
    }
}
