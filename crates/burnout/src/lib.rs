//! The command line of Burnout, and the three device layers under it.
//!
//! The binary holds almost nothing. The work lives here so that a test can
//! reach it, because a test cannot reach into a binary crate.

pub mod cli;
pub mod commands;
pub mod elevate;
pub mod format;
pub mod json;
pub mod platform;
pub mod report;
pub mod stop;

use burnout_core::Error;
use clap::Parser;

use crate::json::Json;

/// Run what the person asked for, and give back the code to exit with.
pub fn run() -> i32 {
    let cli = cli::Cli::parse();
    let outcome = match &cli.command {
        cli::Command::List => commands::list::run(cli.json),
        cli::Command::Write(args) => commands::write::run(args, cli.elevated, cli.json),
    };
    match outcome {
        Ok(code) => code,
        Err(e) => {
            eprintln!("burnout: {e}");
            let note = stop::note(stop::reached(), &e);
            if let Some(note) = note {
                eprintln!("{note}");
            }
            if cli.json {
                println!("{}", error_json(&e, note));
            }
            1
        }
    }
}

/// An error as JSON, for a script: the name of its kind, the message that a
/// person reads, and the note on what the drive holds, when there is one.
fn error_json(e: &Error, note: Option<&str>) -> Json {
    Json::Object(vec![
        ("event", Json::text("error")),
        ("kind", Json::text(e.kind())),
        ("message", Json::text(e.to_string())),
        ("note", Json::maybe_text(note)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_as_json_gives_its_kind_its_message_and_its_note() {
        let e = Error::InUse {
            what: "disk5".to_string(),
        };
        assert_eq!(
            error_json(&e, None).to_string(),
            r#"{"event":"error","kind":"in_use","message":"disk5 is in use. Close each program that uses the drive, and try again","note":null}"#
        );
        let with_note = error_json(&e, Some("The write did not end")).to_string();
        assert!(
            with_note.ends_with(r#""note":"The write did not end"}"#),
            "{with_note}"
        );
    }
}
