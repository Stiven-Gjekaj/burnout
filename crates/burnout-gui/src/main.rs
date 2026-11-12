//! A program with windows that writes a bootable USB drive.
//!
//! It holds no code that writes a drive. It starts `burnout` and shows what
//! `burnout` reports.

// No console window opens beside the program on Windows.
#![windows_subsystem = "windows"]

mod burnout;

use slint::{ModelRc, SharedString, StandardListViewItem, VecModel};

use crate::burnout::{size_text, Burnout, Drive};

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

/// Read the drives again, and show them or what went wrong.
fn refresh(window: &MainWindow, burnout: &Result<Burnout, String>) {
    let result = burnout
        .as_ref()
        .map_err(Clone::clone)
        .and_then(Burnout::list);
    match result {
        Ok(drives) => {
            window.set_drives(rows(&drives));
            window.set_status(match drives.len() {
                1 => "1 drive.".into(),
                n => format!("{n} drives.").into(),
            });
        }
        Err(e) => {
            window.set_drives(rows(&[]));
            window.set_status(e.into());
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let window = MainWindow::new()?;
    let burnout = std::rc::Rc::new(Burnout::locate());
    refresh(&window, &burnout);
    let weak = window.as_weak();
    window.on_refresh(move || {
        if let Some(window) = weak.upgrade() {
            refresh(&window, &burnout);
        }
    });
    window.run()
}
