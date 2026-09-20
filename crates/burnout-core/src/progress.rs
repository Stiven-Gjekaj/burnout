//! How the write path says what it has done.
//!
//! The write path takes one of these and nothing else that talks to a person.
//! A command line gives it a progress bar. A test gives it a closure that
//! collects the events, and then reads them back.

/// The step of the work that an event belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Stage {
    Unmount,
    Write,
    Flush,
    Verify,
}

/// One thing that happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProgressEvent {
    /// The step starts. The total is absent when the code cannot prove it.
    Start {
        stage: Stage,
        total_bytes: Option<u64>,
    },
    /// The step has reached this many bytes.
    Advance { stage: Stage, bytes_done: u64 },
    /// The step is complete, and this many bytes went through it.
    ///
    /// The write path sends this for [`Stage::Write`] only after the flush
    /// returns. A byte count is not a finished write.
    Done { stage: Stage, bytes_done: u64 },
}

/// Something that listens to the write path.
pub trait Progress {
    fn report(&mut self, event: ProgressEvent);
}

impl<F: FnMut(ProgressEvent)> Progress for F {
    fn report(&mut self, event: ProgressEvent) {
        self(event)
    }
}

/// A listener that says nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Silent;

impl Progress for Silent {
    fn report(&mut self, _event: ProgressEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closure_listens_without_a_type_of_its_own() {
        let mut seen = Vec::new();
        {
            let mut collect = |e: ProgressEvent| seen.push(e);
            collect.report(ProgressEvent::Start {
                stage: Stage::Write,
                total_bytes: Some(4096),
            });
            collect.report(ProgressEvent::Done {
                stage: Stage::Write,
                bytes_done: 4096,
            });
        }
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[1],
            ProgressEvent::Done {
                stage: Stage::Write,
                bytes_done: 4096
            }
        );
    }

    #[test]
    fn the_silent_listener_takes_an_event_and_does_nothing() {
        let mut s = Silent;
        s.report(ProgressEvent::Advance {
            stage: Stage::Verify,
            bytes_done: 1,
        });
    }

    #[test]
    fn a_total_is_absent_when_the_code_cannot_prove_one() {
        let e = ProgressEvent::Start {
            stage: Stage::Unmount,
            total_bytes: None,
        };
        assert!(matches!(
            e,
            ProgressEvent::Start {
                total_bytes: None,
                ..
            }
        ));
    }
}
