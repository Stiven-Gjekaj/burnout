//! The JSON that `--json` prints, so that a script reads what a person reads.
//!
//! A small writer, and no dependency. Burnout writes JSON and never reads it,
//! and the whole of the format that it writes fits in this file.

use std::fmt::{self, Write as _};

/// One JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(u64),
    Text(String),
    List(Vec<Json>),
    /// The members of an object, in the order that they print.
    Object(Vec<(&'static str, Json)>),
}

impl Json {
    /// A string.
    pub fn text(text: impl Into<String>) -> Json {
        Json::Text(text.into())
    }

    /// A string, or `null` when there is none.
    pub fn maybe_text(text: Option<&str>) -> Json {
        text.map_or(Json::Null, Json::text)
    }

    /// A number, or `null` when there is none.
    pub fn maybe_number(number: Option<u64>) -> Json {
        number.map_or(Json::Null, Json::Number)
    }
}

impl fmt::Display for Json {
    /// The value on one line, with no space in it but the spaces of a string.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Json::Null => f.write_str("null"),
            Json::Bool(value) => write!(f, "{value}"),
            Json::Number(value) => write!(f, "{value}"),
            Json::Text(text) => quoted(f, text),
            Json::List(items) => {
                f.write_char('[')?;
                for (at, item) in items.iter().enumerate() {
                    if at > 0 {
                        f.write_char(',')?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_char(']')
            }
            Json::Object(members) => {
                f.write_char('{')?;
                for (at, (key, value)) in members.iter().enumerate() {
                    if at > 0 {
                        f.write_char(',')?;
                    }
                    quoted(f, key)?;
                    write!(f, ":{value}")?;
                }
                f.write_char('}')
            }
        }
    }
}

/// A string in quotes, with each character escaped that JSON asks for.
///
/// A drive name comes from the firmware of the drive, so it can hold
/// anything, a quote and a control character included.
fn quoted(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    f.write_char('"')?;
    for c in text.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            c if u32::from(c) < 0x20 || c == '\u{7f}' => write!(f, "\\u{:04x}", u32::from(c))?,
            c => f.write_char(c)?,
        }
    }
    f.write_char('"')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_value_prints_as_json_does() {
        let value = Json::Object(vec![
            ("none", Json::Null),
            ("yes", Json::Bool(true)),
            ("size", Json::Number(128_320_801_792)),
            ("name", Json::text("Flash Drive")),
            (
                "list",
                Json::List(vec![Json::Number(1), Json::Bool(false), Json::Null]),
            ),
            ("empty", Json::List(Vec::new())),
            ("nothing", Json::Object(Vec::new())),
        ]);
        assert_eq!(
            value.to_string(),
            r#"{"none":null,"yes":true,"size":128320801792,"name":"Flash Drive","list":[1,false,null],"empty":[],"nothing":{}}"#
        );
    }

    #[test]
    fn a_string_escapes_each_character_that_json_asks_for() {
        let text = Json::text("a \"b\" c\\d\ne\rf\tg\u{1}h\u{7f}i");
        assert_eq!(text.to_string(), r#""a \"b\" c\\d\ne\rf\tg\u0001h\u007fi""#);
    }

    #[test]
    fn a_string_keeps_its_letters_that_are_not_ascii() {
        assert_eq!(Json::text("Çelës ü").to_string(), "\"Çelës ü\"");
    }

    #[test]
    fn a_missing_text_or_number_prints_null() {
        assert_eq!(Json::maybe_text(None), Json::Null);
        assert_eq!(Json::maybe_text(Some("x")), Json::text("x"));
        assert_eq!(Json::maybe_number(None), Json::Null);
        assert_eq!(Json::maybe_number(Some(4)), Json::Number(4));
    }
}
