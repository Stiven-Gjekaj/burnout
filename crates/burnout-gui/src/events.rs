//! The events of `burnout write --json`, one JSON object on each line.
//!
//! These events are the contract between the two programs. The program reads
//! only the fields that it shows, so a field that a later release adds does
//! no harm.

use serde_json::Value;

use crate::burnout::size_text;

/// One event of a write.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Burnout names the image and the drive, and waits for the answer.
    Confirm {
        /// The image, the mode and the drive, in lines for a person.
        summary: String,
        /// The answer that Burnout takes.
        expects: String,
    },
    /// A step starts. A step with no total has no bar.
    Start { stage: String, total: Option<u64> },
    /// A step advances.
    Progress {
        stage: String,
        done: u64,
        total: Option<u64>,
    },
    /// A step ends.
    Done { stage: String },
    /// The write ended well.
    Result {
        written: u64,
        checked: u64,
        sha256: String,
        ejected: bool,
    },
    /// Burnout stopped on an error.
    Error {
        kind: String,
        message: String,
        note: Option<String>,
    },
    /// A stop ended the write, and the message says what the drive holds.
    Stopped { message: String },
}

/// Read one line of the output. A line that is not an event gives `None`.
pub fn parse(line: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(line).ok()?;
    let text = |name: &str| v[name].as_str().unwrap_or("").to_string();
    let stage = || text("stage");
    Some(match v["event"].as_str()? {
        "confirm" => Event::Confirm {
            summary: summary(&v),
            expects: text("expects"),
        },
        "start" => Event::Start {
            stage: stage(),
            total: v["total_bytes"].as_u64(),
        },
        "progress" => Event::Progress {
            stage: stage(),
            done: v["bytes_done"].as_u64().unwrap_or(0),
            total: v["total_bytes"].as_u64(),
        },
        "done" => Event::Done { stage: stage() },
        "result" => Event::Result {
            written: v["written_bytes"].as_u64().unwrap_or(0),
            checked: v["checked_bytes"].as_u64().unwrap_or(0),
            sha256: text("sha256"),
            ejected: v["ejected"].as_bool().unwrap_or(false),
        },
        "error" => Event::Error {
            kind: text("kind"),
            message: text("message"),
            note: v["note"].as_str().map(str::to_string),
        },
        "stopped" => Event::Stopped {
            message: text("message"),
        },
        _ => return None,
    })
}

/// The lines that a person reads before the answer: the image, the mode, the
/// edition, the drive and the partition, as the confirm event names them.
fn summary(v: &Value) -> String {
    let mut lines = Vec::new();
    let image = &v["image"];
    lines.push(format!(
        "Image: {} ({})",
        image["path"].as_str().unwrap_or("?"),
        size_text(image["size_bytes"].as_u64().unwrap_or(0))
    ));
    let mode = match v["mode"].as_str() {
        Some("raw") => "a copy byte for byte",
        Some("windows") => "the layout of Windows Setup",
        Some(other) => other,
        None => "?",
    };
    lines.push(format!("Mode: {mode}"));
    if let Some(edition) = v["edition"]["name"].as_str() {
        lines.push(format!("Edition: {edition}"));
    }
    let d = &v["drive"];
    let mut drive = format!(
        "Drive: {} ({}), {}",
        d["name"].as_str().unwrap_or("?"),
        size_text(d["size_bytes"].as_u64().unwrap_or(0)),
        d["node"].as_str().unwrap_or("?")
    );
    if let Some(serial) = d["serial"].as_str() {
        drive.push_str(&format!(", serial {serial}"));
    }
    lines.push(drive);
    lines.join("\n")
}

/// A step, in words for a person.
pub fn stage_text(stage: &str) -> &'static str {
    match stage {
        "unmount" => "Unmounting the drive",
        "write" => "Writing",
        "flush" => "Flushing the cache of the drive",
        "verify" => "Checking the drive against the image",
        _ => "Working",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_confirm_event_gives_what_to_show_and_what_to_type() {
        let line = r#"{"event":"confirm","image":{"path":"/i/p.img","size_bytes":8388608},"mode":"raw","edition":null,"drive":{"number":3,"id":"disk6","node":"/dev/rdisk6","name":"Apple Disk Image","serial":null,"size_bytes":67108864},"partition":null,"expects":"yes"}"#;
        let Some(Event::Confirm { summary, expects }) = parse(line) else {
            panic!("confirm")
        };
        assert_eq!(expects, "yes");
        assert_eq!(
            summary,
            "Image: /i/p.img (8.4 MB)\nMode: a copy byte for byte\nDrive: Apple Disk Image (67.1 MB), /dev/rdisk6"
        );
    }

    #[test]
    fn each_kind_of_event_is_read() {
        assert_eq!(
            parse(r#"{"event":"start","stage":"write","total_bytes":209715200}"#),
            Some(Event::Start {
                stage: "write".into(),
                total: Some(209_715_200)
            })
        );
        assert_eq!(
            parse(r#"{"event":"start","stage":"unmount","total_bytes":null}"#),
            Some(Event::Start {
                stage: "unmount".into(),
                total: None
            })
        );
        assert_eq!(
            parse(
                r#"{"event":"progress","stage":"verify","bytes_done":4194304,"total_bytes":8388608}"#
            ),
            Some(Event::Progress {
                stage: "verify".into(),
                done: 4_194_304,
                total: Some(8_388_608)
            })
        );
        assert_eq!(
            parse(r#"{"event":"done","stage":"flush","bytes_done":8388608}"#),
            Some(Event::Done {
                stage: "flush".into()
            })
        );
        assert_eq!(
            parse(
                r#"{"event":"result","mode":"raw","written_bytes":8,"checked_bytes":8,"sha256":"ab","ejected":true,"offline":false}"#
            ),
            Some(Event::Result {
                written: 8,
                checked: 8,
                sha256: "ab".into(),
                ejected: true
            })
        );
        assert_eq!(
            parse(r#"{"event":"error","kind":"in_use","message":"disk5 is in use","note":null}"#),
            Some(Event::Error {
                kind: "in_use".into(),
                message: "disk5 is in use".into(),
                note: None
            })
        );
        assert_eq!(
            parse(r#"{"event":"stopped","reached":"writing","message":"The write did not end"}"#),
            Some(Event::Stopped {
                message: "The write did not end".into()
            })
        );
    }

    #[test]
    fn a_line_that_is_no_event_is_skipped() {
        assert_eq!(parse("Wrote 8 MB"), None);
        assert_eq!(parse(r#"{"event":"later"}"#), None);
        assert_eq!(parse(r#"{"no":"event"}"#), None);
    }
}
