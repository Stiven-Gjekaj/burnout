//! The list command.

use burnout_core::{in_list_order, Result};

use crate::format;
use crate::platform;

/// Print every drive that this computer reports.
pub fn run(json: bool) -> Result<i32> {
    let drives = in_list_order(platform::drive_list()?.drives()?);

    if json {
        println!("{}", format::list_json(&drives));
        return Ok(0);
    }

    if drives.is_empty() {
        // Not an error. The computer answered, and the answer is none.
        println!("No drive found.");
        return Ok(0);
    }

    for line in format::table(&drives) {
        println!("{line}");
    }

    if !drives.iter().any(|d| d.system) {
        // Say so rather than stay quiet. A person who cannot see which drive
        // the system starts from needs to know that Burnout cannot either.
        println!();
        println!("Burnout cannot tell which drive this system starts from.");
    }
    Ok(0)
}
