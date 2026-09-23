//! The text of UDF: CS0, with 8 or 16 bits for each character.

/// Text in CS0: one byte that says 8 or 16, then the characters. A
/// character of 8 bits is the Unicode character of that value. A character
/// of 16 bits has its high byte first.
pub(crate) fn cs0(bytes: &[u8]) -> Result<String, String> {
    let Some((&bits, chars)) = bytes.split_first() else {
        return Ok(String::new());
    };
    match bits {
        8 => Ok(chars.iter().map(|&b| b as char).collect()),
        16 => {
            if chars.len() % 2 != 0 {
                return Err("a text of 16 bits has an odd number of bytes".to_string());
            }
            let units: Vec<u16> = chars
                .chunks_exact(2)
                .map(|p| u16::from_be_bytes([p[0], p[1]]))
                .collect();
            String::from_utf16(&units)
                .map_err(|_| "a text of 16 bits is not valid UTF-16".to_string())
        }
        _ => Err(format!(
            "a text has {bits} bits for each character, and UDF uses 8 or 16"
        )),
    }
}

/// A field of fixed length that holds CS0 text. Its last byte gives the
/// bytes of the text.
pub(crate) fn dstring(field: &[u8]) -> Result<String, String> {
    let Some((&used, room)) = field.split_last() else {
        return Ok(String::new());
    };
    let text = room.get(..used as usize).ok_or_else(|| {
        format!(
            "a field of {} bytes says that its text has {used}",
            field.len()
        )
    })?;
    cs0(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_of_8_bits_is_one_byte_for_each_character() {
        assert_eq!(cs0(b"\x08SOURCES").unwrap(), "SOURCES");
        assert_eq!(cs0(b"\x08caf\xe9").unwrap(), "caf\u{e9}");
        assert_eq!(cs0(b"").unwrap(), "");
    }

    #[test]
    fn text_of_16_bits_has_the_high_byte_first() {
        let mut bytes = vec![16];
        bytes.extend(
            "\u{3053}\u{3093}.txt"
                .encode_utf16()
                .flat_map(|u| u.to_be_bytes()),
        );
        assert_eq!(cs0(&bytes).unwrap(), "\u{3053}\u{3093}.txt");
        assert!(cs0(&[16, 0]).is_err());
        assert!(cs0(&[16, 0xD8, 0x00]).is_err(), "half of a pair");
    }

    #[test]
    fn another_number_of_bits_is_refused() {
        assert!(cs0(&[254, b'a']).unwrap_err().contains("254 bits"));
    }

    #[test]
    fn the_label_of_a_windows_iso_is_read() {
        // The logical volume of Win11_25H2_English_x64_v2.iso keeps its label
        // in 16 bits, and the last byte of the field gives 47.
        let mut field = vec![16];
        field.extend(
            "CCCOMA_X64FRE_EN-US_DV9"
                .encode_utf16()
                .flat_map(|u| u.to_be_bytes()),
        );
        field.resize(127, 0);
        field.push(47);
        assert_eq!(dstring(&field).unwrap(), "CCCOMA_X64FRE_EN-US_DV9");
    }

    #[test]
    fn a_field_that_says_more_than_it_holds_is_refused() {
        assert!(dstring(&[8, b'a', 9])
            .unwrap_err()
            .contains("says that its text has 9"));
        assert_eq!(dstring(&[0; 32]).unwrap(), "");
    }
}
