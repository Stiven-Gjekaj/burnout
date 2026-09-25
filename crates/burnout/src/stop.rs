//! What a person learns when a write stops before its end.
//!
//! Burnout resumes nothing and promises nothing about a stopped write. It
//! says what the drive holds now, because a drive that holds part of an image
//! starts nothing, and its old data is gone too.
//!
//! The write reports each step to a listener. [`Tracked`] records how far the
//! work on the drive got. The error path of the command reads it back, and so
//! does the handler that [`on_stop`] puts in place for Ctrl-C.

use std::sync::atomic::{AtomicU8, Ordering};

use burnout_core::{Error, Progress, ProgressEvent, Stage};

/// How far the work on the drive got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reached {
    /// Nothing is written to the drive.
    Nothing,
    /// The write started, and it did not end.
    Writing,
    /// The write and its flush ended, and the check did not start.
    Written,
    /// The check started, and it did not end.
    Checking,
    /// The check ended.
    Checked,
}

impl Reached {
    fn code(self) -> u8 {
        match self {
            Reached::Nothing => 0,
            Reached::Writing => 1,
            Reached::Written => 2,
            Reached::Checking => 3,
            Reached::Checked => 4,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => Reached::Writing,
            2 => Reached::Written,
            3 => Reached::Checking,
            4 => Reached::Checked,
            _ => Reached::Nothing,
        }
    }
}

/// How far the work of this process got. One process writes one drive.
static REACHED: AtomicU8 = AtomicU8::new(0);

/// How far the work of this process got.
pub fn reached() -> Reached {
    Reached::from_code(REACHED.load(Ordering::SeqCst))
}

/// How far the work is after one event, when the event moves it.
///
/// The write reports its end only after the flush, so a drive is written when
/// the write is done, and not when its byte count reached the image.
pub fn after(event: ProgressEvent) -> Option<Reached> {
    match event {
        ProgressEvent::Start {
            stage: Stage::Write,
            ..
        } => Some(Reached::Writing),
        ProgressEvent::Done {
            stage: Stage::Write,
            ..
        } => Some(Reached::Written),
        ProgressEvent::Start {
            stage: Stage::Verify,
            ..
        } => Some(Reached::Checking),
        ProgressEvent::Done {
            stage: Stage::Verify,
            ..
        } => Some(Reached::Checked),
        _ => None,
    }
}

/// A listener that records how far the work got, and then passes each event
/// on.
pub struct Tracked<P>(pub P);

impl<P: Progress> Progress for Tracked<P> {
    fn report(&mut self, event: ProgressEvent) {
        if let Some(now) = after(event) {
            REACHED.store(now.code(), Ordering::SeqCst);
        }
        self.0.report(event);
    }
}

/// What a drive holds when the write stopped part of the way, as a literal
/// that `concat!` takes.
macro_rules! part {
    () => {
        "The write did not end, so the drive holds part of the image, and it is not usable now"
    };
}

/// What a drive holds when the check stopped part of the way.
macro_rules! unchecked {
    () => {
        "The drive holds the whole image, but Burnout did not complete the check"
    };
}

const PART: &str = part!();
const UNCHECKED: &str = unchecked!();

/// The line that follows the error that stopped a write, when the drive
/// holds something that the error does not say.
///
/// A check that found a difference says what the drive holds itself.
pub fn note(reached: Reached, e: &Error) -> Option<&'static str> {
    if matches!(e, Error::VerifyFailed { .. } | Error::VolumeDiffers { .. }) {
        return None;
    }
    match reached {
        Reached::Writing => Some(PART),
        Reached::Written | Reached::Checking => Some(UNCHECKED),
        Reached::Nothing | Reached::Checked => None,
    }
}

/// The line that a stop by a signal prints, by how far the work got.
///
/// Each line starts with a line break, which ends a progress line that the
/// stop leaves open, and it ends with one.
pub fn stopped(reached: Reached) -> &'static str {
    match reached {
        Reached::Nothing => "\nburnout: stopped. Burnout wrote nothing to the drive\n",
        Reached::Writing => concat!("\nburnout: stopped. ", part!(), ". Write the image again\n"),
        Reached::Written | Reached::Checking => concat!(
            "\nburnout: stopped. ",
            unchecked!(),
            ". Write the image again to check it\n"
        ),
        Reached::Checked => {
            "\nburnout: stopped. The drive holds the image, and the check proved it\n"
        }
    }
}

/// Say what the drive holds when a signal stops the write, and end.
///
/// Ctrl-C, a `kill`, and a terminal that closes each stop the write. Without
/// this, the process ends with no word, and a person has no way to know that
/// the drive now starts nothing. The handler does only what a signal allows:
/// it reads one atomic, writes one static line, and ends the process with
/// 128 and the number of the signal, as a shell does.
#[cfg(unix)]
pub fn on_stop() {
    extern "C" fn stop(signal: libc::c_int) {
        let text = stopped(reached());
        // SAFETY: `write` and `_exit` are safe in a signal handler, and the
        // text is static.
        unsafe {
            libc::write(libc::STDERR_FILENO, text.as_ptr().cast(), text.len());
            libc::_exit(128 + signal);
        }
    }
    let handler = stop as extern "C" fn(libc::c_int);
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler touches one atomic, `write` and `_exit`, and
        // nothing else.
        unsafe { libc::signal(signal, handler as libc::sighandler_t) };
    }
}

/// Say what the drive holds when a signal stops the write. This host has no
/// handler yet, and a stop ends the process with no word.
#[cfg(not(unix))]
pub fn on_stop() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_work_moves_on_the_start_and_the_end_of_the_write_and_the_check() {
        let start = |stage| ProgressEvent::Start {
            stage,
            total_bytes: None,
        };
        let done = |stage| ProgressEvent::Done {
            stage,
            bytes_done: 0,
        };
        assert_eq!(after(start(Stage::Unmount)), None);
        assert_eq!(after(start(Stage::Write)), Some(Reached::Writing));
        // The flush is part of the write. A drive whose flush did not end
        // holds part of the image.
        assert_eq!(after(start(Stage::Flush)), None);
        assert_eq!(after(done(Stage::Flush)), None);
        assert_eq!(after(done(Stage::Write)), Some(Reached::Written));
        assert_eq!(after(start(Stage::Verify)), Some(Reached::Checking));
        assert_eq!(after(done(Stage::Verify)), Some(Reached::Checked));
    }

    #[test]
    fn each_step_reads_back_as_itself() {
        for r in [
            Reached::Nothing,
            Reached::Writing,
            Reached::Written,
            Reached::Checking,
            Reached::Checked,
        ] {
            assert_eq!(Reached::from_code(r.code()), r);
        }
    }

    fn fault() -> Error {
        Error::Io(std::io::Error::other("gone"))
    }

    #[test]
    fn a_write_that_stopped_says_that_the_drive_is_not_usable() {
        assert_eq!(note(Reached::Writing, &fault()), Some(PART));
        assert!(PART.contains("not usable"));
    }

    #[test]
    fn a_check_that_stopped_says_that_nothing_proves_the_drive() {
        assert_eq!(note(Reached::Written, &fault()), Some(UNCHECKED));
        assert_eq!(note(Reached::Checking, &fault()), Some(UNCHECKED));
    }

    #[test]
    fn an_error_before_the_write_or_after_the_check_gets_no_note() {
        assert_eq!(note(Reached::Nothing, &fault()), None);
        assert_eq!(note(Reached::Checked, &fault()), None);
    }

    #[test]
    fn a_check_that_found_a_difference_says_it_itself() {
        let differs = Error::VerifyFailed {
            expected: "aa".to_string(),
            got: "bb".to_string(),
            at_byte: None,
        };
        assert_eq!(note(Reached::Checking, &differs), None);
        let volume = Error::VolumeDiffers {
            path: "the partition table".to_string(),
            detail: "x".to_string(),
        };
        assert_eq!(note(Reached::Written, &volume), None);
    }

    #[test]
    fn a_stop_says_what_the_drive_holds_at_each_step() {
        assert_eq!(
            stopped(Reached::Nothing),
            "\nburnout: stopped. Burnout wrote nothing to the drive\n"
        );
        assert_eq!(
            stopped(Reached::Writing),
            "\nburnout: stopped. The write did not end, so the drive holds part of the image, \
             and it is not usable now. Write the image again\n"
        );
        for r in [Reached::Written, Reached::Checking] {
            assert_eq!(
                stopped(r),
                "\nburnout: stopped. The drive holds the whole image, but Burnout did not \
                 complete the check. Write the image again to check it\n"
            );
        }
        assert!(stopped(Reached::Checked).contains("the check proved it"));
    }

    #[test]
    fn a_tracked_listener_records_the_step_and_passes_the_event_on() {
        let mut seen = Vec::new();
        {
            let mut tracked = Tracked(|e: ProgressEvent| seen.push(e));
            tracked.report(ProgressEvent::Start {
                stage: Stage::Write,
                total_bytes: Some(1),
            });
            assert_eq!(reached(), Reached::Writing);
        }
        assert_eq!(seen.len(), 1);
    }
}
