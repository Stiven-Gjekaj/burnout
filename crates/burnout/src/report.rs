//! What a person sees while a drive is written.
//!
//! The line itself comes out of a pure function, so a test reads it on any
//! host. The part that puts it on a terminal carries one rule: a line that a
//! failed step leaves open ends before the error prints.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use burnout_core::{human_size, Progress, ProgressEvent, Stage};

use crate::json::Json;

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
///
/// A flush carries no rate at all. It moves the bytes that the write handed
/// to the host and that the host still holds, and the host does not say how
/// many those are. Measured on Linux: 2.7 GB divided by a flush of 9 seconds
/// gave 303 MB/s for a stick that reads back at 27 MB/s.
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
    if stage != Stage::Flush && seconds >= 1.0 && done > 0 {
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

/// When each step started, and the total that it gave at the start.
///
/// The write path reports the end of the write only after the flush returns,
/// because a byte count is not a finished write. So the time of the write
/// runs from its start, across the flush, to that report. One clock that each
/// step restarts divides the write by the time of the flush alone.
///
/// Only the start of a step carries its total. A line drawn while the step
/// runs takes the total from here, or it shows no percentage and no time left.
#[derive(Debug, Default)]
struct Starts(Vec<(Stage, Instant, Option<u64>)>);

impl Starts {
    fn begin(&mut self, stage: Stage, now: Instant, total: Option<u64>) {
        self.0.retain(|(s, _, _)| *s != stage);
        self.0.push((stage, now, total));
    }

    fn elapsed(&self, stage: Stage, now: Instant) -> Duration {
        self.0
            .iter()
            .find(|(s, _, _)| *s == stage)
            .map_or(Duration::ZERO, |(_, at, _)| {
                now.saturating_duration_since(*at)
            })
    }

    fn total(&self, stage: Stage) -> Option<u64> {
        self.0
            .iter()
            .find(|(s, _, _)| *s == stage)
            .and_then(|(_, _, total)| *total)
    }
}

/// A progress line on the terminal.
///
/// It draws on the error stream, so that the output of Burnout stays
/// something a script can read. It draws nothing at all when the error stream
/// is not a terminal, because a carriage return in a log file gives one line
/// of nonsense per drive.
pub struct Bar {
    starts: Starts,
    drawn: Instant,
    width: usize,
    /// Where the line goes, or nothing when the error stream is not a
    /// terminal.
    screen: Option<Box<dyn Write>>,
}

impl Default for Bar {
    fn default() -> Self {
        Self::new()
    }
}

impl Bar {
    pub fn new() -> Self {
        let err = std::io::stderr();
        let screen: Option<Box<dyn Write>> = if err.is_terminal() {
            Some(Box::new(err))
        } else {
            None
        };
        Self::on(screen)
    }

    fn on(screen: Option<Box<dyn Write>>) -> Self {
        let now = Instant::now();
        Bar {
            starts: Starts::default(),
            drawn: now - REDRAW,
            width: 0,
            screen,
        }
    }

    /// Put one line down, over the last one.
    fn draw(&mut self, line: &str) {
        let Some(screen) = self.screen.as_mut() else {
            return;
        };
        // Cover whatever the last line left behind, so a shorter line does
        // not end with the tail of a longer one.
        let padding = self.width.saturating_sub(line.chars().count());
        let _ = write!(screen, "\r{line}{:padding$}", "");
        let _ = screen.flush();
        self.width = line.chars().count();
    }

    /// Draw the last form of a line, and end it.
    fn finish(&mut self, line: &str) {
        self.draw(line);
        self.end_line();
    }

    /// End the line that is on the screen, if a line is open.
    fn end_line(&mut self) {
        if self.width == 0 {
            return;
        }
        if let Some(screen) = self.screen.as_mut() {
            let _ = writeln!(screen);
            let _ = screen.flush();
        }
        self.width = 0;
    }
}

impl Drop for Bar {
    /// A step that fails does not finish its line. Without this, the error
    /// prints on the end of it: `write 0% 0 B of 2.7 GBburnout: Incorrect
    /// function.`
    fn drop(&mut self) {
        self.end_line();
    }
}

impl Progress for Bar {
    fn report(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::Start { stage, total_bytes } => {
                let now = Instant::now();
                self.starts.begin(stage, now, total_bytes);
                self.drawn = now - REDRAW;
                let line = progress_line(stage, 0, total_bytes, Duration::ZERO);
                self.draw(&line);
            }
            ProgressEvent::Advance { stage, bytes_done } => {
                let now = Instant::now();
                if now.duration_since(self.drawn) < REDRAW {
                    return;
                }
                self.drawn = now;
                let elapsed = self.starts.elapsed(stage, now);
                let total = self.starts.total(stage);
                let line = progress_line(stage, bytes_done, total, elapsed);
                self.draw(&line);
            }
            ProgressEvent::Done { stage, bytes_done } => {
                let elapsed = self.starts.elapsed(stage, Instant::now());
                let line = progress_line(stage, bytes_done, Some(bytes_done), elapsed);
                self.finish(&line);
            }
            _ => {}
        }
    }
}

/// The progress of a write as JSON, one object on each line of the output
/// stream, for a script to read.
///
/// The start and the end of each step always print. An advance prints four
/// times a second at most, as the line for a person does, and it carries the
/// total that the start of its step gave.
pub struct JsonProgress {
    starts: Starts,
    printed: Instant,
    out: Box<dyn Write>,
}

impl Default for JsonProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonProgress {
    pub fn new() -> Self {
        Self::on(Box::new(std::io::stdout()))
    }

    fn on(out: Box<dyn Write>) -> Self {
        JsonProgress {
            starts: Starts::default(),
            printed: Instant::now() - REDRAW,
            out,
        }
    }

    fn print(&mut self, value: Json) {
        let _ = writeln!(self.out, "{value}");
        let _ = self.out.flush();
    }
}

impl Progress for JsonProgress {
    fn report(&mut self, event: ProgressEvent) {
        let now = Instant::now();
        let value = match event {
            ProgressEvent::Start { stage, total_bytes } => {
                self.starts.begin(stage, now, total_bytes);
                Json::Object(vec![
                    ("event", Json::text("start")),
                    ("stage", Json::text(stage_name(stage))),
                    ("total_bytes", Json::maybe_number(total_bytes)),
                ])
            }
            ProgressEvent::Advance { stage, bytes_done } => {
                if now.duration_since(self.printed) < REDRAW {
                    return;
                }
                self.printed = now;
                Json::Object(vec![
                    ("event", Json::text("progress")),
                    ("stage", Json::text(stage_name(stage))),
                    ("bytes_done", Json::Number(bytes_done)),
                    ("total_bytes", Json::maybe_number(self.starts.total(stage))),
                ])
            }
            ProgressEvent::Done { stage, bytes_done } => Json::Object(vec![
                ("event", Json::text("done")),
                ("stage", Json::text(stage_name(stage))),
                ("bytes_done", Json::Number(bytes_done)),
            ]),
            _ => return,
        };
        self.print(value);
    }
}

/// What a write reports to: the line for a person, or JSON for a script.
pub enum Listener {
    Bar(Bar),
    Json(JsonProgress),
}

impl Progress for Listener {
    fn report(&mut self, event: ProgressEvent) {
        match self {
            Listener::Bar(bar) => bar.report(event),
            Listener::Json(json) => json.report(event),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;

    /// A terminal that keeps what it gets.
    #[derive(Clone, Default)]
    struct Screen(Rc<RefCell<Vec<u8>>>);

    impl Screen {
        fn text(&self) -> String {
            String::from_utf8(self.0.borrow().clone()).unwrap()
        }
    }

    impl Write for Screen {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_step_that_fails_ends_its_line_before_the_error_prints() {
        let screen = Screen::default();
        let mut bar = Bar::on(Some(Box::new(screen.clone())));
        bar.report(ProgressEvent::Start {
            stage: Stage::Write,
            total_bytes: Some(2_700_000_000),
        });
        // The error goes up past the bar, and the bar goes with it.
        drop(bar);
        let text = screen.text();
        assert!(text.starts_with("\rwrite"), "{text:?}");
        assert!(text.ends_with('\n'), "{text:?}");
    }

    #[test]
    fn a_step_under_way_shows_how_much_of_it_is_done() {
        let screen = Screen::default();
        let mut bar = Bar::on(Some(Box::new(screen.clone())));
        bar.report(ProgressEvent::Start {
            stage: Stage::Write,
            total_bytes: Some(2_000_000_000),
        });
        bar.report(ProgressEvent::Advance {
            stage: Stage::Write,
            bytes_done: 500_000_000,
        });
        let text = screen.text();
        let last = text.rsplit('\r').next().unwrap();
        assert!(last.contains("25%"), "{text:?}");
        assert!(last.contains("of 2.0 GB"), "{text:?}");
    }

    #[test]
    fn a_finished_step_leaves_no_empty_line_behind() {
        let screen = Screen::default();
        let mut bar = Bar::on(Some(Box::new(screen.clone())));
        bar.report(ProgressEvent::Start {
            stage: Stage::Unmount,
            total_bytes: None,
        });
        bar.report(ProgressEvent::Done {
            stage: Stage::Unmount,
            bytes_done: 0,
        });
        drop(bar);
        assert_eq!(
            screen.text().matches('\n').count(),
            1,
            "{:?}",
            screen.text()
        );
    }

    #[test]
    fn the_progress_as_json_gives_each_start_and_end_and_the_total_of_an_advance() {
        let screen = Screen::default();
        let mut json = JsonProgress::on(Box::new(screen.clone()));
        json.report(ProgressEvent::Start {
            stage: Stage::Write,
            total_bytes: Some(2_000),
        });
        json.report(ProgressEvent::Advance {
            stage: Stage::Write,
            bytes_done: 500,
        });
        // Too soon after the last one, so it does not print.
        json.report(ProgressEvent::Advance {
            stage: Stage::Write,
            bytes_done: 600,
        });
        json.report(ProgressEvent::Done {
            stage: Stage::Write,
            bytes_done: 2_000,
        });
        assert_eq!(
            screen.text(),
            concat!(
                r#"{"event":"start","stage":"write","total_bytes":2000}"#,
                "\n",
                r#"{"event":"progress","stage":"write","bytes_done":500,"total_bytes":2000}"#,
                "\n",
                r#"{"event":"done","stage":"write","bytes_done":2000}"#,
                "\n"
            )
        );
    }

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
    fn a_flush_carries_no_rate() {
        // Measured on Linux: all 2.7 GB over a flush of 9 seconds read as
        // 303 MB/s, for a stick that reads back at 27 MB/s.
        let line = progress_line(
            Stage::Flush,
            2_689_781_760,
            Some(2_689_781_760),
            Duration::from_secs(9),
        );
        assert!(line.contains("100%"));
        assert!(!line.contains("/s"), "{line}");
    }

    #[test]
    fn the_write_is_timed_from_its_own_start_across_the_flush() {
        // The write ends only when the flush returns, so its rate is the
        // bytes over the whole time, and not over the flush that came last.
        let t0 = Instant::now();
        let mut starts = Starts::default();
        starts.begin(Stage::Write, t0, None);
        starts.begin(Stage::Flush, t0 + Duration::from_secs(120), None);
        let end = t0 + Duration::from_secs(129);
        assert_eq!(starts.elapsed(Stage::Flush, end), Duration::from_secs(9));
        assert_eq!(starts.elapsed(Stage::Write, end), Duration::from_secs(129));

        let bytes = 2_689_781_760;
        let line = progress_line(
            Stage::Write,
            bytes,
            Some(bytes),
            starts.elapsed(Stage::Write, end),
        );
        assert!(line.contains("20.9 MB/s"), "{line}");
    }

    #[test]
    fn a_step_that_starts_again_is_timed_from_its_new_start() {
        let t0 = Instant::now();
        let mut starts = Starts::default();
        starts.begin(Stage::Verify, t0, None);
        starts.begin(Stage::Verify, t0 + Duration::from_secs(5), None);
        let end = t0 + Duration::from_secs(7);
        assert_eq!(starts.elapsed(Stage::Verify, end), Duration::from_secs(2));
        assert_eq!(starts.elapsed(Stage::Unmount, end), Duration::ZERO);
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
