//! What a person sees while a drive is written.
//!
//! The line itself comes out of a pure function, so a test reads it on any
//! host. The part that puts it on a terminal is three lines at the bottom and
//! carries no rule.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use burnout_core::{Progress, ProgressEvent, Stage};

use crate::format::human_size;

/// How often the line is drawn.
///
/// A drive takes minutes and a terminal is not a film. Four times a second is
/// enough to look alive and rare enough that the drawing costs nothing.
const REDRAW: Duration = Duration::from_millis(250);

/// The name of a step, as a person reads it.
pub fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Unmount => "unmount",
        Stage::Write => "write",
        Stage::Flush => "flush",
        Stage::Verify => "verify",
        // `Stage` is marked non_exhaustive, so a step added later needs a
        // name and not a panic.
        _ => "work",
    }
}

/// One line of progress.
///
/// The rate and the time left come from what has happened and not from a
/// guess about what will. Neither appears until there is enough to divide by,
/// because a rate over no time is not a rate.
pub fn progress_line(stage: Stage, done: u64, total: Option<u64>, elapsed: Duration) -> String {
    let mut line = format!("{:<8}", stage_name(stage));

    match total {
        Some(total) if total > 0 => {
            let percent = (done as f64 / total as f64 * 100.0).min(100.0);
            line.push_str(&format!(
                "{percent:>3.0}%  {} of {}",
                human_size(done),
                human_size(total)
            ));
        }
        _ => line.push_str(&human_size(done)),
    }

    let seconds = elapsed.as_secs_f64();
    if seconds >= 1.0 && done > 0 {
        let rate = done as f64 / seconds;
        line.push_str(&format!("   {}/s", human_size(rate as u64)));
        if let Some(total) = total {
            if total > done {
                let left = (total - done) as f64 / rate;
                line.push_str(&format!("   {} left", duration(left)));
            }
        }
    }
    line
}

/// A number of seconds, as a person says it.
fn duration(seconds: f64) -> String {
    let seconds = seconds.round() as u64;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// A progress line on the terminal.
///
/// It draws on the error stream, so that the output of Burnout stays
/// something a script can read. It draws nothing at all when the error stream
/// is not a terminal, because a carriage return in a log file gives one line
/// of nonsense per drive.
pub struct Bar {
    started: Instant,
    drawn: Instant,
    width: usize,
    quiet: bool,
}

impl Default for Bar {
    fn default() -> Self {
        Self::new()
    }
}

impl Bar {
    pub fn new() -> Self {
        let now = Instant::now();
        Bar {
            started: now,
            drawn: now - REDRAW,
            width: 0,
            quiet: !std::io::stderr().is_terminal(),
        }
    }

    /// Put one line down, over the last one.
    fn draw(&mut self, line: &str) {
        if self.quiet {
            return;
        }
        // Cover whatever the last line left behind, so a shorter line does
        // not end with the tail of a longer one.
        let padding = self.width.saturating_sub(line.chars().count());
        let mut err = std::io::stderr();
        let _ = write!(err, "\r{line}{:padding$}", "");
        let _ = err.flush();
        self.width = line.chars().count();
    }

    /// End the line that is on the screen.
    fn finish(&mut self, line: &str) {
        if self.quiet {
            return;
        }
        self.draw(line);
        let _ = writeln!(std::io::stderr());
        self.width = 0;
    }
}

impl Progress for Bar {
    fn report(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::Start { stage, total_bytes } => {
                self.started = Instant::now();
                self.drawn = self.started - REDRAW;
                let line = progress_line(stage, 0, total_bytes, Duration::ZERO);
                self.draw(&line);
            }
            ProgressEvent::Advance { stage, bytes_done } => {
                let now = Instant::now();
                if now.duration_since(self.drawn) < REDRAW {
                    return;
                }
                self.drawn = now;
                let line = progress_line(stage, bytes_done, None, now - self.started);
                self.draw(&line);
            }
            ProgressEvent::Done { stage, bytes_done } => {
                let elapsed = Instant::now() - self.started;
                let line = progress_line(stage, bytes_done, Some(bytes_done), elapsed);
                self.finish(&line);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_with_a_total_carries_the_percentage() {
        let line = progress_line(
            Stage::Write,
            500_000_000,
            Some(1_000_000_000),
            Duration::ZERO,
        );
        assert!(line.starts_with("write"));
        assert!(line.contains("50%"));
        assert!(line.contains("500.0 MB of 1.0 GB"));
    }

    #[test]
    fn a_line_with_no_total_still_says_how_far_it_got() {
        let line = progress_line(Stage::Verify, 1_500_000, None, Duration::ZERO);
        assert!(line.contains("1.5 MB"));
        assert!(!line.contains('%'));
    }

    #[test]
    fn no_rate_appears_before_there_is_a_second_to_divide_by() {
        // A rate over no time is not a rate. Printing one would be a number
        // that the code cannot prove.
        let line = progress_line(Stage::Write, 1_000_000, Some(2_000_000), Duration::ZERO);
        assert!(!line.contains("/s"));
    }

    #[test]
    fn a_rate_and_a_time_left_appear_once_there_is_enough_to_measure() {
        let line = progress_line(
            Stage::Write,
            100_000_000,
            Some(300_000_000),
            Duration::from_secs(10),
        );
        assert!(line.contains("10.0 MB/s"));
        assert!(line.contains("20s left"));
    }

    #[test]
    fn a_finished_step_says_no_time_left() {
        let line = progress_line(
            Stage::Write,
            300_000_000,
            Some(300_000_000),
            Duration::from_secs(30),
        );
        assert!(line.contains("100%"));
        assert!(!line.contains("left"));
    }

    #[test]
    fn a_total_of_zero_does_not_divide_by_zero() {
        let line = progress_line(Stage::Write, 0, Some(0), Duration::from_secs(5));
        assert!(!line.contains("NaN"));
        assert!(!line.contains("inf"));
    }

    #[test]
    fn a_count_past_the_total_does_not_print_more_than_all_of_it() {
        let line = progress_line(Stage::Write, 20, Some(10), Duration::ZERO);
        assert!(line.contains("100%"));
    }

    #[test]
    fn a_long_wait_reads_in_minutes_and_hours() {
        assert_eq!(duration(45.0), "45s");
        assert_eq!(duration(90.0), "1m 30s");
        assert_eq!(duration(3725.0), "1h 02m");
    }

    #[test]
    fn every_step_has_a_name_and_none_of_them_panics() {
        for stage in [Stage::Unmount, Stage::Write, Stage::Flush, Stage::Verify] {
            assert!(!stage_name(stage).is_empty());
        }
    }
}
