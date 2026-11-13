//! Read the WIM and ESD images of Windows.
//!
//! A Windows ISO holds its system in `sources/install.wim`, or in
//! `sources/install.esd`, which is a WIM file with LZMS compression. One file
//! holds many editions, such as Home and Pro. This crate reads the header and
//! the XML that lists the editions. The XML is not compressed, so no
//! decompressor is needed for it.
//!
//! wimlib is LGPL or GPL, so the reader here is new code, to keep Burnout
//! under the MIT licence. The one exception is the LZMS decompressor in
//! `lzms`, a port of the one of wimlib that its author allowed under the MIT
//! licence; that module says more. Microsoft documents the format in
//! "Windows Imaging File Format (WIM)".

#![forbid(unsafe_code)]

mod blobs;
mod header;
mod huffman;
pub mod lzms;
mod lzx;
mod metadata;
mod resource;
pub mod sha1;
mod xml;
mod xpress;

use std::io::{Read, Seek, SeekFrom};

use burnout_core::{Error, Result};

pub use blobs::{parse_blobs, Blob};
pub use header::{Compression, Header, Resource, HEADER_BYTES};
pub use metadata::{
    parse_metadata, Dentry, ExtraStream, Metadata, ATTRIBUTE_DIRECTORY, ATTRIBUTE_REPARSE_POINT,
};
pub use resource::{copy_resource, read_resource, BlobReader};

/// The largest XML that Burnout reads. An image with many editions holds
/// some tens of KB.
const XML_LIMIT: u64 = 16 * 1024 * 1024;

/// The processor that an edition runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86,
    Arm,
    Ia64,
    X64,
    Arm64,
    /// A number that the format does not name.
    Other(u32),
}

impl Arch {
    fn from_number(n: u32) -> Arch {
        match n {
            0 => Arch::X86,
            5 => Arch::Arm,
            6 => Arch::Ia64,
            9 => Arch::X64,
            12 => Arch::Arm64,
            n => Arch::Other(n),
        }
    }

    pub fn name(self) -> String {
        match self {
            Arch::X86 => "x86".to_string(),
            Arch::Arm => "ARM".to_string(),
            Arch::Ia64 => "IA-64".to_string(),
            Arch::X64 => "x64".to_string(),
            Arch::Arm64 => "ARM64".to_string(),
            Arch::Other(n) => format!("processor {n}"),
        }
    }
}

/// One edition of a WIM file, as its XML gives it. A field that the XML does
/// not give is empty, or `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edition {
    /// The number that Setup and DISM give the edition, from 1.
    pub index: u32,
    pub name: String,
    pub description: String,
    /// The name that Setup shows, when it differs from `name`.
    pub display_name: String,
    /// Such as "Professional" or "Core".
    pub edition_id: String,
    pub arch: Option<Arch>,
    /// Such as "10.0.26100.1742".
    pub version: String,
    pub languages: Vec<String>,
    pub default_language: String,
    /// The bytes of the files of the edition, before compression.
    pub total_bytes: Option<u64>,
    pub file_count: Option<u64>,
    pub dir_count: Option<u64>,
}

impl Edition {
    /// The name to show a person: the display name, or the name.
    pub fn shown_name(&self) -> &str {
        if self.display_name.is_empty() {
            &self.name
        } else {
            &self.display_name
        }
    }
}

/// A WIM file: its header and its editions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wim {
    pub header: Header,
    pub editions: Vec<Edition>,
}

impl Wim {
    /// The edition that `text` names: its index, its name or display name,
    /// its edition ID, or the end of its name after a space, such as "Pro" for
    /// "Windows 11 Pro". Case does not matter. A text that names more than one
    /// edition names none.
    pub fn find_edition(&self, text: &str) -> Option<&Edition> {
        let text = text.trim();
        if let Ok(index) = text.parse::<u32>() {
            return self.editions.iter().find(|e| e.index == index);
        }
        let same = |a: &str| !a.is_empty() && a.eq_ignore_ascii_case(text);
        let tests: [&dyn Fn(&Edition) -> bool; 3] = [
            &|e| same(&e.name) || same(&e.display_name),
            &|e| same(&e.edition_id),
            &|e| {
                [&e.name, &e.display_name].iter().any(|n| {
                    let lower = n.to_ascii_lowercase();
                    lower.ends_with(&format!(" {}", text.to_ascii_lowercase()))
                })
            },
        ];
        for test in tests {
            let found: Vec<&Edition> = self.editions.iter().filter(|e| test(e)).collect();
            match found.len() {
                0 => continue,
                1 => return Some(found[0]),
                _ => return None,
            }
        }
        None
    }
}

/// Read the header and the editions of the WIM file that `reader` gives.
/// `name` names the file in an error.
pub fn read_wim<R: Read + Seek>(mut reader: R, name: &str) -> Result<Wim> {
    let fault = |detail: String| Error::Image {
        path: name.to_string(),
        detail,
    };
    let mut head = vec![0u8; HEADER_BYTES];
    reader.seek(SeekFrom::Start(0))?;
    read_full(&mut reader, &mut head)
        .map_err(|_| fault("it is shorter than a WIM header".to_string()))?;
    let header = Header::parse(&head, name)?;

    let xml = header.xml;
    if xml.stored_bytes == 0 {
        return Err(fault("its WIM header gives no XML".to_string()));
    }
    if xml.is_compressed() {
        return Err(fault(
            "its XML is compressed, and Burnout reads XML that is not compressed only".to_string(),
        ));
    }
    if xml.stored_bytes > XML_LIMIT {
        return Err(fault(format!(
            "its XML holds {} bytes, more than the {XML_LIMIT} that Burnout reads",
            xml.stored_bytes
        )));
    }
    let mut bytes = vec![0u8; xml.stored_bytes as usize];
    reader.seek(SeekFrom::Start(xml.offset))?;
    read_full(&mut reader, &mut bytes).map_err(|_| {
        fault(format!(
            "it ends before the end of its XML at byte {}",
            xml.offset + xml.stored_bytes
        ))
    })?;
    let text = utf16_text(&bytes).ok_or_else(|| fault("its XML is not UTF-16".to_string()))?;
    let top = xml::parse(&text).map_err(|e| fault(format!("its XML is damaged: {e}")))?;
    let editions = editions(&top).map_err(fault)?;
    if editions.len() != header.image_count as usize {
        return Err(fault(format!(
            "its WIM header counts {} images, and its XML lists {}",
            header.image_count,
            editions.len()
        )));
    }
    Ok(Wim { header, editions })
}

/// Read the offset table of the WIM file that `reader` gives, with the
/// header that `read_wim` read.
pub fn read_blobs<R: Read + Seek>(mut reader: R, header: &Header, name: &str) -> Result<Vec<Blob>> {
    let bytes = read_resource(&mut reader, header, &header.offset_table, name)?;
    parse_blobs(&bytes, name)
}

fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<()> {
    reader.read_exact(buf)
}

/// The text of UTF-16LE bytes, with or without a byte order mark.
fn utf16_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let units = units.strip_prefix(&[0xFEFF]).unwrap_or(&units);
    String::from_utf16(units).ok()
}

fn editions(top: &xml::Element) -> std::result::Result<Vec<Edition>, String> {
    if !top.name.eq_ignore_ascii_case("WIM") {
        return Err(format!("its XML starts with <{}>, and not <WIM>", top.name));
    }
    let mut editions = Vec::new();
    for image in top.children_named("IMAGE") {
        let index = image
            .attribute("INDEX")
            .and_then(|i| i.parse().ok())
            .ok_or("an <IMAGE> in its XML has no INDEX")?;
        let text = |path: &[&str]| image.text_at(path).unwrap_or("").to_string();
        let number = |path: &[&str]| image.text_at(path).and_then(|t| t.parse::<u64>().ok());
        let windows = image.child("WINDOWS");
        let version = windows
            .and_then(|w| {
                let part = |n: &str| w.text_at(&["VERSION", n]);
                Some(format!(
                    "{}.{}.{}.{}",
                    part("MAJOR")?,
                    part("MINOR")?,
                    part("BUILD")?,
                    part("SPBUILD").unwrap_or("0")
                ))
            })
            .unwrap_or_default();
        let languages = windows
            .and_then(|w| w.child("LANGUAGES"))
            .map(|l| {
                l.children_named("LANGUAGE")
                    .map(|e| e.text.clone())
                    .collect()
            })
            .unwrap_or_default();
        editions.push(Edition {
            index,
            name: text(&["NAME"]),
            description: text(&["DESCRIPTION"]),
            display_name: text(&["DISPLAYNAME"]),
            edition_id: text(&["WINDOWS", "EDITIONID"]),
            arch: number(&["WINDOWS", "ARCH"]).map(|n| Arch::from_number(n as u32)),
            version,
            languages,
            default_language: text(&["WINDOWS", "LANGUAGES", "DEFAULT"]),
            total_bytes: number(&["TOTALBYTES"]),
            file_count: number(&["FILECOUNT"]),
            dir_count: number(&["DIRCOUNT"]),
        });
    }
    editions.sort_by_key(|e| e.index);
    for (i, e) in editions.iter().enumerate() {
        if e.index as usize != i + 1 {
            return Err(format!(
                "the images in its XML are not numbered 1 to {}",
                editions.len()
            ));
        }
    }
    Ok(editions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const XML: &str = r#"<WIM><TOTALBYTES>5000</TOTALBYTES>
<IMAGE INDEX="2"><DIRCOUNT>20</DIRCOUNT><FILECOUNT>100</FILECOUNT><TOTALBYTES>2000</TOTALBYTES>
<WINDOWS><ARCH>9</ARCH><EDITIONID>Professional</EDITIONID>
<LANGUAGES><LANGUAGE>en-US</LANGUAGE><LANGUAGE>de-DE</LANGUAGE><DEFAULT>en-US</DEFAULT></LANGUAGES>
<VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>19041</BUILD><SPBUILD>2006</SPBUILD></VERSION></WINDOWS>
<NAME>Windows 10 Pro</NAME><DESCRIPTION>Windows 10 Pro</DESCRIPTION><DISPLAYNAME>Windows 10 Pro</DISPLAYNAME></IMAGE>
<IMAGE INDEX="1"><TOTALBYTES>1000</TOTALBYTES><WINDOWS><ARCH>12</ARCH><EDITIONID>Core</EDITIONID></WINDOWS>
<NAME>Windows 10 Home</NAME></IMAGE></WIM>"#;

    /// A WIM file with a header, some bytes of resources, and `xml` as
    /// UTF-16LE with a byte order mark.
    fn wim(image_count: u32, xml: &str) -> Vec<u8> {
        let mut text = vec![0xFF, 0xFE];
        for u in xml.encode_utf16() {
            text.extend_from_slice(&u.to_le_bytes());
        }
        let offset = (HEADER_BYTES + 1000) as u64;
        let mut file = header::tests::header(0x0004_0002, image_count, (offset, text.len() as u64));
        file.resize(offset as usize, 0xAB);
        file.extend_from_slice(&text);
        file
    }

    #[test]
    fn the_editions_come_from_the_xml_in_the_order_of_their_index() {
        let w = read_wim(Cursor::new(wim(2, XML)), "install.wim").unwrap();
        assert_eq!(w.header.compression(), Compression::Lzx);
        assert_eq!(w.editions.len(), 2);
        let home = &w.editions[0];
        assert_eq!(home.index, 1);
        assert_eq!(home.shown_name(), "Windows 10 Home");
        assert_eq!(home.edition_id, "Core");
        assert_eq!(home.arch, Some(Arch::Arm64));
        assert_eq!(home.version, "");
        assert_eq!(home.total_bytes, Some(1000));
        assert_eq!(home.file_count, None);
        let pro = &w.editions[1];
        assert_eq!(pro.index, 2);
        assert_eq!(pro.arch, Some(Arch::X64));
        assert_eq!(pro.arch.unwrap().name(), "x64");
        assert_eq!(pro.version, "10.0.19041.2006");
        assert_eq!(pro.languages, ["en-US", "de-DE"]);
        assert_eq!(pro.default_language, "en-US");
        assert_eq!(
            (pro.total_bytes, pro.file_count, pro.dir_count),
            (Some(2000), Some(100), Some(20))
        );
    }

    #[test]
    fn an_edition_is_found_by_its_index_its_name_or_the_end_of_its_name() {
        let edition = |index, name: &str, id: &str| Edition {
            index,
            name: name.to_string(),
            edition_id: id.to_string(),
            ..Edition::default()
        };
        let w = Wim {
            header: Header::parse(&header::tests::header(0, 4, (0, 0)), "a").unwrap(),
            editions: vec![
                edition(1, "Windows 11 Home", "Core"),
                edition(2, "Windows 11 Home N", "CoreN"),
                edition(3, "Windows 11 Pro", "Professional"),
                edition(4, "Windows 11 Pro N", "ProfessionalN"),
            ],
        };
        let index = |text: &str| w.find_edition(text).map(|e| e.index);
        assert_eq!(index("3"), Some(3));
        assert_eq!(index(" 4 "), Some(4));
        assert_eq!(index("5"), None);
        assert_eq!(index("windows 11 pro"), Some(3));
        assert_eq!(index("Professional"), Some(3));
        assert_eq!(index("coren"), Some(2));
        assert_eq!(index("Pro"), Some(3));
        assert_eq!(index("pro n"), Some(4));
        assert_eq!(index("N"), None, "N ends two names");
        assert_eq!(index("Education"), None);
        assert_eq!(index(""), None);
    }

    #[test]
    fn a_name_is_shown_when_there_is_no_display_name() {
        let mut e = Edition {
            name: "A".to_string(),
            ..Edition::default()
        };
        assert_eq!(e.shown_name(), "A");
        e.display_name = "B".to_string();
        assert_eq!(e.shown_name(), "B");
    }

    #[test]
    fn a_count_that_does_not_match_the_header_is_refused() {
        let e = read_wim(Cursor::new(wim(3, XML)), "install.wim").unwrap_err();
        assert!(
            e.to_string()
                .contains("counts 3 images, and its XML lists 2"),
            "{e}"
        );
    }

    #[test]
    fn a_damaged_wim_says_what_is_wrong() {
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (vec![1, 2, 3], "shorter than a WIM header"),
            (
                wim(1, "<WIM><IMAGE INDEX=\"1\"></WIM>"),
                "its XML is damaged",
            ),
            (wim(1, "<X/>"), "starts with <X>"),
            (wim(1, "<WIM><IMAGE/></WIM>"), "has no INDEX"),
            (
                wim(1, "<WIM><IMAGE INDEX=\"2\"/></WIM>"),
                "not numbered 1 to 1",
            ),
            (
                wim(1, XML)[..HEADER_BYTES + 1100].to_vec(),
                "ends before the end of its XML",
            ),
        ];
        for (bytes, says) in cases {
            let e = read_wim(Cursor::new(bytes), "install.wim").unwrap_err();
            assert!(e.to_string().contains(says), "{says}: {e}");
            assert_eq!(e.kind(), "image");
        }
    }

    #[test]
    fn a_compressed_or_missing_xml_is_refused() {
        let mut b = wim(2, XML);
        b[0x4F] |= Resource::COMPRESSED;
        let e = read_wim(Cursor::new(b), "a.esd").unwrap_err();
        assert!(e.to_string().contains("its XML is compressed"), "{e}");

        let b = header::tests::header(0, 1, (0, 0));
        let e = read_wim(Cursor::new(b), "a.wim").unwrap_err();
        assert!(e.to_string().contains("gives no XML"), "{e}");
    }

    #[test]
    fn utf16_with_and_without_a_mark_is_read() {
        assert_eq!(utf16_text(&[0xFF, 0xFE, b'a', 0]).unwrap(), "a");
        assert_eq!(utf16_text(&[b'a', 0, b'b', 0]).unwrap(), "ab");
        assert_eq!(utf16_text(b"a"), None);
        assert_eq!(utf16_text(&[0x00, 0xD8]), None);
    }
}
