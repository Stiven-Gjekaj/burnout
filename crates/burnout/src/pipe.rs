//! The two named pipes of `--pipe`. They take the place of the streams for a
//! process that UAC starts, because such a process gets no streams from the
//! program that started it.

/// The longest name that `--pipe` takes. Windows allows 256 characters for
/// the whole path, and the path adds `\\.\pipe\` and `.out`.
const LONGEST: usize = 200;

/// Check a name that a person gave to `--pipe`.
///
/// A name holds letters, digits, `-`, `_` and `.` only. A backslash would
/// put the pipe in a different place, so a name cannot hold one.
pub fn name(text: &str) -> Result<String, String> {
    if text.is_empty() || text.len() > LONGEST {
        return Err(format!("give a name of 1 to {LONGEST} characters"));
    }
    if let Some(c) = text
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(format!(
            "{c:?} is not allowed. Use letters, digits, '-', '_' and '.'"
        ));
    }
    Ok(text.to_string())
}

/// The paths of the two pipes of a name: the events go out through the
/// first, and the answer comes in through the second.
pub fn paths(name: &str) -> (String, String) {
    (
        format!(r"\\.\pipe\{name}.out"),
        format!(r"\\.\pipe\{name}.in"),
    )
}

/// Open the two pipes, and make them the output and the input of this
/// process.
///
/// The standard library asks Windows for the handle of a stream each time
/// it reads or writes it, so each event and the answer go through the pipes
/// from here on. Call this before anything reads or writes those streams.
#[cfg(windows)]
pub fn connect(name: &str) -> std::io::Result<()> {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};

    let (out, input) = paths(name);
    let out = std::fs::OpenOptions::new().write(true).open(out)?;
    let input = std::fs::File::open(input)?;
    for (stream, file) in [(STD_OUTPUT_HANDLE, out), (STD_INPUT_HANDLE, input)] {
        // SAFETY: the handle is open, and the process keeps it until it
        // ends, because the stream uses it from now on.
        if unsafe { SetStdHandle(stream, file.into_raw_handle()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Whether a line from the program asks for a stop.
pub fn is_stop(line: &str) -> bool {
    line.trim().eq_ignore_ascii_case("stop")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_of_letters_digits_and_marks_is_taken() {
        assert_eq!(name("burnout-gui_42.a").unwrap(), "burnout-gui_42.a");
    }

    #[test]
    fn a_name_that_could_leave_the_folder_of_pipes_is_refused() {
        for bad in [r"..\x", "a/b", "a b", "", "é"] {
            assert!(name(bad).is_err(), "{bad:?}");
        }
        assert!(name(&"a".repeat(LONGEST)).is_ok());
        assert!(name(&"a".repeat(LONGEST + 1)).is_err());
    }

    #[test]
    fn only_the_word_stop_stops_the_write() {
        assert!(is_stop("stop"));
        assert!(is_stop(" STOP\r"));
        assert!(!is_stop("yes"));
        assert!(!is_stop("stopped"));
    }

    #[test]
    fn a_name_gives_one_pipe_out_and_one_pipe_in() {
        assert_eq!(
            paths("gui"),
            (
                r"\\.\pipe\gui.out".to_string(),
                r"\\.\pipe\gui.in".to_string()
            )
        );
    }
}
