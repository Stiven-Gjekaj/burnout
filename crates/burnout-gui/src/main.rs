//! A program with windows that writes a bootable USB drive.
//!
//! It holds no code that writes a drive. It starts `burnout` with the
//! privilege, shows the events that `burnout` reports, and passes on the
//! answer that the person types.

// No console window opens beside the program on Windows.
#![windows_subsystem = "windows"]

mod burnout;
mod child;
mod events;

use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, ModelRc, SharedString, StandardListViewItem, VecModel};

use crate::burnout::{size_text, Burnout, Drive};
use crate::child::Ending;
use crate::events::{stage_text, Event};

slint::include_modules!();

/// The rows of the table, one for each drive.
fn rows(drives: &[Drive]) -> ModelRc<ModelRc<StandardListViewItem>> {
    let rows: Vec<ModelRc<StandardListViewItem>> = drives
        .iter()
        .map(|d| {
            let cells: Vec<StandardListViewItem> = [
                d.number.to_string(),
                d.name.clone(),
                size_text(d.size_bytes),
                d.bus.clone(),
                d.kind().to_string(),
            ]
            .into_iter()
            .map(|text| StandardListViewItem::from(SharedString::from(text)))
            .collect();
            ModelRc::new(VecModel::from(cells))
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

/// What the program knows between two events.
#[derive(Default)]
struct State {
    drives: Vec<Drive>,
    image: Option<PathBuf>,
    /// The last event that ends a write: a result, an error or a stop.
    outcome: Option<Event>,
    /// The person cancelled at the confirmation.
    cancelled_at_confirm: bool,
}

/// Where the answer goes while a write runs. The thread of the write puts
/// it here, and the window takes it for the answer and for a stop.
type Answer = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

/// Whether the write button can go, and the line that says why or why not.
fn choice(state: &State, row: i32) -> (bool, String) {
    let drive = usize::try_from(row).ok().and_then(|r| state.drives.get(r));
    match (&state.image, drive) {
        (None, _) => (false, "Choose the image to write.".into()),
        (Some(_), None) => (false, "Choose the drive to erase.".into()),
        (Some(_), Some(d)) if d.system => (
            false,
            "The system starts from this disk. Burnout never writes it.".into(),
        ),
        (Some(_), Some(d)) => (
            true,
            format!(
                "Burnout erases {} ({}). Windows To Go, a partition, and the options of Setup are on the command line only.",
                d.name,
                size_text(d.size_bytes)
            ),
        ),
    }
}

fn update_choice(window: &MainWindow, state: &State) {
    let (can, status) = choice(state, window.get_selected_row());
    window.set_can_write(can);
    window.set_status(status.into());
}

/// Read the drives again, and show them or what went wrong.
fn refresh(window: &MainWindow, burnout: &Result<Burnout, String>, state: &mut State) {
    match burnout
        .as_ref()
        .map_err(Clone::clone)
        .and_then(Burnout::list)
    {
        Ok(drives) => state.drives = drives,
        Err(e) => {
            state.drives.clear();
            window.set_drives(rows(&[]));
            window.set_selected_row(-1);
            window.set_can_write(false);
            window.set_status(e.into());
            return;
        }
    }
    window.set_drives(rows(&state.drives));
    window.set_selected_row(-1);
    update_choice(window, state);
}

/// Show one event of the write.
fn show(window: &MainWindow, state: &mut State, event: Event) {
    match event {
        Event::Confirm { summary, expects } => {
            window.set_confirm_text(summary.into());
            window.set_expects(expects.into());
            window.set_typed("".into());
            window.set_page(Page::Confirm);
        }
        Event::Start { stage, total } => {
            window.set_page(Page::Running);
            window.set_stage_text(stage_text(&stage).into());
            window.set_progress(0.0);
            window.set_progress_known(total.is_some());
            window.set_progress_text(
                total
                    .map(|t| format!("0 of {}", size_text(t)))
                    .unwrap_or_default()
                    .into(),
            );
        }
        Event::Progress { stage, done, total } => {
            window.set_stage_text(stage_text(&stage).into());
            if let Some(total) = total.filter(|t| *t > 0) {
                let part = done as f64 / total as f64;
                window.set_progress_known(true);
                window.set_progress(part as f32);
                window.set_progress_text(
                    format!(
                        "{} of {} ({:.0} %)",
                        size_text(done),
                        size_text(total),
                        part * 100.0
                    )
                    .into(),
                );
            } else {
                window.set_progress_text(size_text(done).into());
            }
        }
        Event::Done { .. } => {
            window.set_progress(1.0);
        }
        ending @ (Event::Result { .. } | Event::Error { .. } | Event::Stopped { .. }) => {
            state.outcome = Some(ending);
        }
    }
}

/// The title and the text of the last page.
fn end(state: &State, ending: &Result<Ending, String>) -> (String, String) {
    match (&state.outcome, ending) {
        (_, Err(e)) => ("Burnout did not start".into(), e.clone()),
        (_, Ok(Ending::Cancelled)) => (
            "Cancelled".into(),
            "The dialog that asks for the privilege was closed, so Burnout wrote nothing.".into(),
        ),
        (Some(Event::Result { written, checked, sha256, ejected }), _) => (
            "The drive is ready".into(),
            format!(
                "Burnout wrote {} and checked {} of the drive against the image, byte for byte.\nSHA-256 {sha256}\n{}",
                size_text(*written),
                size_text(*checked),
                if *ejected {
                    "The host ejected the drive, so you can unplug it."
                } else {
                    "Eject the drive before you unplug it."
                }
            ),
        ),
        (Some(Event::Error { kind, .. }), _) if kind == "not_confirmed" && state.cancelled_at_confirm => (
            "Cancelled".into(),
            "You cancelled before the write, so Burnout wrote nothing.".into(),
        ),
        (Some(Event::Error { message, note, .. }), _) => (
            "Burnout stopped".into(),
            match note {
                Some(note) => format!("{message}.\n{note}"),
                None => format!("{message}."),
            },
        ),
        (Some(Event::Stopped { message }), _) => ("Stopped".into(), message.clone()),
        (_, Ok(Ending::Code(code))) => (
            "Burnout ended".into(),
            format!("Burnout ended with the code {code}, and said nothing about the drive."),
        ),
    }
}

/// Start the write on a thread of its own, and send each event to the
/// window.
fn start_write(
    window: &MainWindow,
    burnout: PathBuf,
    image: PathBuf,
    node: String,
    answer: Answer,
) {
    window.set_page(Page::Running);
    window.set_stage_text("Starting Burnout".into());
    window.set_progress_known(false);
    window.set_progress_text("The host can ask for your password first.".into());
    window.set_stopping(false);
    let weak = window.as_weak();
    std::thread::spawn(move || {
        let ending = child::start(&burnout, &image, &node).map(|mut child| {
            *answer.lock().unwrap() = Some(std::mem::replace(
                &mut child.answer,
                Box::new(std::io::sink()),
            ));
            let mut line = String::new();
            loop {
                line.clear();
                match std::io::BufRead::read_line(&mut child.events, &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if let Some(event) = events::parse(&line) {
                    let weak = weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = weak.upgrade() {
                            STATE.with(|s| show(&w, &mut s.borrow_mut(), event));
                        }
                    });
                }
            }
            *answer.lock().unwrap() = None;
            child.wait()
        });
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                STATE.with(|s| {
                    let s = s.borrow();
                    let (title, text) = end(&s, &ending);
                    w.set_end_title(title.into());
                    w.set_end_text(text.into());
                });
                w.set_page(Page::Finished);
            }
        });
    });
}

thread_local! {
    /// The state lives on the thread of the window, and only there.
    static STATE: Rc<RefCell<State>> = Rc::new(RefCell::new(State::default()));
}

fn main() -> Result<(), slint::PlatformError> {
    let window = MainWindow::new()?;
    let burnout = Rc::new(Burnout::locate());
    let answer: Answer = Arc::new(Mutex::new(None));
    STATE.with(|s| refresh(&window, &burnout, &mut s.borrow_mut()));

    let weak = window.as_weak();
    let b = burnout.clone();
    window.on_refresh(move || {
        let w = weak.unwrap();
        STATE.with(|s| refresh(&w, &b, &mut s.borrow_mut()));
    });

    let weak = window.as_weak();
    window.on_row_chosen(move |_| {
        let w = weak.unwrap();
        STATE.with(|s| update_choice(&w, &s.borrow()));
    });

    let weak = window.as_weak();
    window.on_choose_image(move || {
        let w = weak.unwrap();
        let picked = rfd::FileDialog::new()
            .set_title("Choose the image to write")
            .add_filter("Disk images", &["iso", "img", "wim", "raw", "bin", "dd"])
            .add_filter("All files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            w.set_image_path(path.display().to_string().into());
            STATE.with(|s| {
                s.borrow_mut().image = Some(path);
                update_choice(&w, &s.borrow());
            });
        }
    });

    let weak = window.as_weak();
    let b = burnout.clone();
    let a = answer.clone();
    window.on_start_write(move || {
        let w = weak.unwrap();
        let Ok(burnout) = b.as_ref() else { return };
        let picked = STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.outcome = None;
            s.cancelled_at_confirm = false;
            let drive = usize::try_from(w.get_selected_row())
                .ok()
                .and_then(|r| s.drives.get(r));
            match (&s.image, drive) {
                (Some(image), Some(d)) if !d.system => Some((image.clone(), d.node.clone())),
                _ => None,
            }
        });
        if let Some((image, node)) = picked {
            start_write(&w, burnout.path().to_path_buf(), image, node, a.clone());
        }
    });

    let weak = window.as_weak();
    let a = answer.clone();
    window.on_send_answer(move |text| {
        let w = weak.unwrap();
        if let Some(out) = a.lock().unwrap().as_mut() {
            let _ = writeln!(out, "{}", text.trim());
            let _ = out.flush();
        }
        w.set_page(Page::Running);
        w.set_stage_text("Starting the write".into());
        w.set_progress_known(false);
        w.set_progress_text("The host can ask for your password now.".into());
    });

    let weak = window.as_weak();
    let a = answer.clone();
    window.on_cancel(move || {
        let w = weak.unwrap();
        let mut guard = a.lock().unwrap();
        if w.get_page() == Page::Confirm {
            // An empty answer is not the answer, so Burnout writes nothing.
            STATE.with(|s| s.borrow_mut().cancelled_at_confirm = true);
            if let Some(out) = guard.as_mut() {
                let _ = writeln!(out);
                let _ = out.flush();
            }
            w.set_page(Page::Running);
            w.set_stage_text("Cancelling".into());
        } else if let Some(out) = guard.as_mut() {
            let _ = writeln!(out, "stop");
            let _ = out.flush();
            w.set_stopping(true);
        }
    });

    let weak = window.as_weak();
    let b = burnout.clone();
    window.on_back(move || {
        let w = weak.unwrap();
        w.set_page(Page::Choose);
        STATE.with(|s| refresh(&w, &b, &mut s.borrow_mut()));
    });

    window.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(system: bool) -> Drive {
        Drive {
            number: 2,
            name: "SanDisk".into(),
            node: "/dev/rdisk4".into(),
            size_bytes: 32_000_000_000,
            bus: "USB".into(),
            removable: true,
            system,
        }
    }

    #[test]
    fn the_write_waits_for_an_image_and_a_drive_that_is_not_the_system() {
        let mut s = State::default();
        assert!(!choice(&s, -1).0);
        s.image = Some("u.iso".into());
        assert!(!choice(&s, -1).0);
        s.drives = vec![drive(true), drive(false)];
        let (can, why) = choice(&s, 0);
        assert!(!can);
        assert!(why.contains("never writes it"), "{why}");
        assert!(choice(&s, 1).0);
        assert!(!choice(&s, 5).0);
    }

    #[test]
    fn the_last_page_names_what_ended_the_write() {
        let mut s = State::default();
        assert_eq!(end(&s, &Ok(Ending::Cancelled)).0, "Cancelled");
        assert_eq!(end(&s, &Ok(Ending::Code(1))).0, "Burnout ended");
        s.outcome = Some(Event::Error {
            kind: "not_confirmed".into(),
            message: "Not confirmed".into(),
            note: None,
        });
        assert_eq!(end(&s, &Ok(Ending::Code(1))).0, "Burnout stopped");
        s.cancelled_at_confirm = true;
        assert_eq!(end(&s, &Ok(Ending::Code(1))).0, "Cancelled");
        s.outcome = Some(Event::Stopped {
            message: "The write did not end".into(),
        });
        assert_eq!(
            end(&s, &Ok(Ending::Code(130))),
            ("Stopped".into(), "The write did not end".into())
        );
        s.outcome = Some(Event::Result {
            written: 8,
            checked: 8,
            sha256: "ab".into(),
            ejected: true,
        });
        let (title, text) = end(&s, &Ok(Ending::Code(0)));
        assert_eq!(title, "The drive is ready");
        assert!(text.contains("SHA-256 ab"), "{text}");
    }
}
