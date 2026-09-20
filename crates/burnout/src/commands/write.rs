//! The write command.
//!
//! The order of the steps is the point of this file, and
//! [the milestones](../../../../docs/milestones.md) fix it:
//!
//! ```text
//! open and measure the image          no privilege
//! list the drives, find the target    no privilege
//! ----------------------------------  check the privilege, start again here
//! confirm the target with the person
//! list again, and refuse a drive that changed
//! unmount, write, flush, verify
//! ```
//!
//! The privilege comes after the image check, so nobody gives a password and
//! then learns that the path is wrong. It comes before the confirmation, so
//! the person confirms once and not once for each process.

use std::fs::File;
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};

use burnout_core::{
    check_fits, check_same_drive, check_target, describe, force_phrase, has_boot_table,
    in_list_order, phrase_matches, verify_image, write_image, DriveAccess, DriveInfo, Error, Force,
    Progress, ProgressEvent, Result, Stage, BOOT_SECTOR_BYTES,
};

use crate::cli::WriteArgs;
use crate::elevate::{self, Plan, State};
use crate::format::size_column;
use crate::platform;
use crate::report::Bar;

/// Write an image to a drive.
pub fn run(args: &WriteArgs, elevated: bool) -> Result<i32> {
    let mut image = File::open(&args.image).map_err(|e| Error::Image {
        path: args.image.display().to_string(),
        detail: e.to_string(),
    })?;
    let image_bytes = image.seek(SeekFrom::End(0))?;
    image.seek(SeekFrom::Start(0))?;
    if image_bytes == 0 {
        return Err(Error::Image {
            path: args.image.display().to_string(),
            detail: "the file holds no bytes".to_string(),
        });
    }
    let boot_table = first_sector_has_boot_table(&mut image)?;

    let force = if args.force { Force::Yes } else { Force::No };
    let chosen = find_target(args)?;
    check_target(&chosen, force)?;
    check_fits(image_bytes, chosen.size_bytes)?;

    let state = State {
        privileged: elevate::privileged(),
        already_elevated: elevated,
        no_elevate: args.no_elevate,
        interactive: std::io::stdin().is_terminal(),
        can_restart: cfg!(unix),
    };
    match elevate::plan(state) {
        Plan::GoAhead => {}
        Plan::Stop => return Err(elevate::refusal(state)),
        Plan::StartAgain => {
            eprintln!("Burnout needs root to write a drive, so it asks sudo for it.");
            elevate::start_again(&std::env::args().skip(1).collect::<Vec<_>>())?;
            unreachable!("the restart replaces this program");
        }
    }

    if !confirm(&chosen, force, image_bytes, args, boot_table)? {
        return Err(Error::NotConfirmed);
    }

    // The confirmation takes a person's time, and a drive can leave in it.
    // A number is a position in a list, so the drive at that number now may
    // not be the drive that was named a moment ago.
    let again = find_target(args)?;
    check_same_drive(&chosen, &again)?;
    check_target(&again, force)?;

    let access = platform::drive_access()?;
    let mut bar = Bar::new();

    bar.report(ProgressEvent::Start {
        stage: Stage::Unmount,
        total_bytes: None,
    });
    access.unmount_volumes(&again.id)?;
    bar.report(ProgressEvent::Done {
        stage: Stage::Unmount,
        bytes_done: 0,
    });

    let report = {
        let mut target = access.open(&again.id)?;
        write_image(&mut image, &mut target, &mut bar)?
    };

    // Open the drive again for the check. A read through the handle that did
    // the writing can come out of the cache of the operating system, and then
    // it proves that the cache holds the image and not that the drive does.
    let proof = {
        let mut target = access.open(&again.id)?;
        verify_image(&mut image, &mut target, &mut bar)?
    };

    println!();
    println!(
        "Wrote {} to {}.",
        size_column(report.image_bytes),
        again.name
    );
    println!(
        "Checked {} of the drive against the image, byte for byte.",
        size_column(proof.bytes)
    );
    println!("SHA-256 {}", proof.digest);
    Ok(0)
}

/// Read the first sector and say whether it carries a boot table.
fn first_sector_has_boot_table(image: &mut File) -> Result<bool> {
    let mut first = vec![0u8; BOOT_SECTOR_BYTES];
    let mut filled = 0;
    while filled < first.len() {
        match image.read(&mut first[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    image.seek(SeekFrom::Start(0))?;
    Ok(has_boot_table(&first[..filled]))
}

/// The drive that the person named, out of the list as it is now.
///
/// A path is allowed for a script, and it still has to name a drive that the
/// list reports. Without that, every refusal in this file would have nothing
/// to run against.
fn find_target(args: &WriteArgs) -> Result<DriveInfo> {
    let drives = in_list_order(platform::drive_list()?.drives()?);

    if let Some(path) = &args.device {
        return drives
            .into_iter()
            .find(|d| d.node == *path || d.id.as_str() == path)
            .ok_or_else(|| Error::NoSuchDrive {
                wanted: path.clone(),
            });
    }

    // clap asks for one of these, so this is the case where a later change
    // took that rule away.
    let Some(index) = args.target else {
        return Err(Error::NoSuchDrive {
            wanted: "nothing. Give the number that burnout list printed".to_string(),
        });
    };
    burnout_core::by_index(&drives, index)
        .cloned()
        .ok_or_else(|| Error::NoSuchDrive {
            wanted: index.to_string(),
        })
}

/// Show the target and wait for the person to agree to it.
///
/// A drive that Burnout cannot prove is removable asks for the model and the
/// size of the drive, which a person can only produce by looking at the drive
/// in front of them. An ordinary drive asks for the whole word `yes`, because
/// the person already named it by number.
fn confirm(
    drive: &DriveInfo,
    force: Force,
    image_bytes: u64,
    args: &WriteArgs,
    boot_table: bool,
) -> Result<bool> {
    println!("This erases the drive. Nothing undoes it.");
    println!();
    println!("    image   {}", args.image.display());
    println!("            {}", size_column(image_bytes));
    println!("    drive   {}", drive.name);
    println!("            {}", size_column(drive.size_bytes));
    println!(
        "            {} {} {}",
        drive.node,
        drive.bus.label(),
        if drive.removable() {
            "removable"
        } else {
            "fixed"
        }
    );
    if let Some(serial) = &drive.serial {
        println!("            serial {serial}");
    }
    println!();
    if boot_table {
        println!("The image carries a boot table in its first sector.");
    } else {
        println!(
            "The image carries no boot table in its first sector, so the drive \
             may start nothing."
        );
    }
    println!();

    let wanted = if force == Force::Yes {
        println!("This drive is not one that Burnout can prove is removable.");
        println!("Type the model and the size of the drive to go on:");
        println!("    {}", force_phrase(drive));
        None
    } else {
        println!("Type yes to go on:");
        Some("yes")
    };
    print!("> ");
    std::io::stdout().flush()?;

    let mut typed = String::new();
    std::io::stdin().read_line(&mut typed)?;

    let agreed = match wanted {
        Some(word) => typed.trim().eq_ignore_ascii_case(word),
        None => phrase_matches(&typed, drive),
    };
    if !agreed {
        println!("Nothing was written to {}.", describe(drive));
    }
    Ok(agreed)
}
