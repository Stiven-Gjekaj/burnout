//! The write command.
//!
//! The order of the steps is the point of this file, and
//! [the milestones](../../../../docs/milestones.md) fix it:
//!
//! ```text
//! read the image, choose the mode     no privilege
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
use std::path::Path;

use burnout_core::{
    check_fits, check_same_drive, check_target, describe, force_phrase, has_boot_table,
    in_list_order, phrase_matches, verify_image, write_image, DriveAccess, DriveInfo, Error, Force,
    Progress, ProgressEvent, Result, Stage, BOOT_SECTOR_BYTES,
};
use burnout_iso::{mode_of_file, Mode};

use crate::cli::{ModeArg, WriteArgs};
use crate::elevate::{self, Plan, State};
use crate::format::size_column;
use crate::platform;
use crate::report::Bar;

/// Write an image to a drive.
pub fn run(args: &WriteArgs, elevated: bool) -> Result<i32> {
    if args.mode != Some(ModeArg::Raw) {
        check_mode(mode_of_file(&args.image), &args.image)?;
    }
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
    let (drives, chosen) = find_target(args)?;
    // A host that cannot name its own system disk cannot refuse it either.
    // A live USB does this: the root is an overlay, the medium is reached
    // through a loop device, and nothing in between names the drive.
    let system_disk_known = drives.iter().any(|d| d.system);
    check_target(&chosen, force)?;
    check_fits(image_bytes, chosen.size_bytes)?;

    let state = State {
        privileged: elevate::can_write(&chosen.node),
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

    if !confirm(
        &chosen,
        force,
        image_bytes,
        args,
        boot_table,
        system_disk_known,
    )? {
        return Err(Error::NotConfirmed);
    }

    // The confirmation takes a person's time, and a drive can leave in it.
    // A number is a position in a list, so the drive at that number now may
    // not be the drive that was named a moment ago.
    let (_, again) = find_target(args)?;
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
    // The Windows layer took the drive offline for the write, and a person
    // who looks for it in Explorer needs to know why it is not there.
    #[cfg(windows)]
    println!("Windows now holds the drive offline and read-only, so it changes nothing on it.");
    Ok(0)
}

/// Go on only with an image that asks for raw mode.
///
/// A Windows ISO asks for Windows mode, which a later phase writes. An image
/// that fits neither mode stops, and so does a file whose contents Burnout
/// cannot read, and each error says how to copy it anyway. A path that is
/// not a file gives the fault alone, because no mode copies it.
fn check_mode(mode: Result<Mode>, image: &Path) -> Result<()> {
    let path = image.display().to_string();
    match mode {
        Ok(Mode::Raw) => Ok(()),
        Ok(Mode::Windows) => Err(Error::WindowsMode { path }),
        Ok(Mode::Neither) => Err(Error::NoMode { path }),
        Err(Error::Image { path, detail }) if image.is_file() => Err(Error::Image {
            path,
            detail: format!("{detail}. To copy it byte for byte anyway, use --mode raw"),
        }),
        Err(e) => Err(e),
    }
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
fn find_target(args: &WriteArgs) -> Result<(Vec<DriveInfo>, DriveInfo)> {
    let drives = in_list_order(platform::drive_list()?.drives()?);

    if let Some(path) = &args.device {
        let found = drives
            .iter()
            .find(|d| names_drive(d, path))
            .cloned()
            .ok_or_else(|| Error::NoSuchDrive {
                wanted: path.clone(),
            })?;
        return Ok((drives, found));
    }

    // clap asks for one of these, so this is the case where a later change
    // took that rule away.
    let Some(index) = args.target else {
        return Err(Error::NoSuchDrive {
            wanted: "nothing. Give the number that burnout list printed".to_string(),
        });
    };
    let found = burnout_core::by_index(&drives, index)
        .cloned()
        .ok_or_else(|| Error::NoSuchDrive {
            wanted: index.to_string(),
        })?;
    Ok((drives, found))
}

/// Whether a path from `--device` names this drive.
///
/// It names the drive by the node that Burnout writes, by the name of the
/// drive, or by that name under `/dev`. The last one is for macOS, where every
/// tool of the system prints `/dev/disk5` and Burnout writes `/dev/rdisk5`.
/// Measured: `--device /dev/disk5` answered that no drive carries the name.
fn names_drive(drive: &DriveInfo, path: &str) -> bool {
    drive.node == path
        || drive.id.as_str() == path
        || path.strip_prefix("/dev/") == Some(drive.id.as_str())
}

/// Show the target and wait for the person to agree to it.
///
/// Two prompts, and the harder one is not only for `--force`.
///
/// A drive that Burnout cannot prove is removable asks for the model and the
/// size of the drive, which a person can only produce by looking at the drive
/// in front of them. So does every drive on a host where Burnout could not
/// find the system disk, because there the refusal that protects that disk
/// did not run, and the drive in front of the person may be the one their
/// computer is running from. A live USB is exactly that case.
///
/// An ordinary drive on a host that named its system disk asks for the whole
/// word `yes`, because the person already named the drive by number.
fn confirm(
    drive: &DriveInfo,
    force: Force,
    image_bytes: u64,
    args: &WriteArgs,
    boot_table: bool,
    system_disk_known: bool,
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
    } else if !system_disk_known {
        println!("Burnout cannot tell which drive this system starts from, so it");
        println!("cannot refuse that drive. Read the drive above, and type its");
        println!("model and its size to go on:");
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

#[cfg(test)]
mod tests {
    use super::*;
    use burnout_core::DriveId;

    #[test]
    fn an_image_that_asks_for_raw_mode_goes_on() {
        assert!(check_mode(Ok(Mode::Raw), Path::new("ubuntu.iso")).is_ok());
    }

    #[test]
    fn a_windows_iso_is_refused_and_the_message_names_the_way_past() {
        let e = check_mode(Ok(Mode::Windows), Path::new("Win11.iso")).unwrap_err();
        assert!(matches!(e, Error::WindowsMode { .. }), "{e}");
        assert!(e.to_string().contains("--mode raw"), "{e}");
    }

    #[test]
    fn an_image_that_fits_neither_mode_is_refused_and_the_message_names_the_way_past() {
        let e = check_mode(Ok(Mode::Neither), Path::new("disk.img.xz")).unwrap_err();
        assert!(matches!(e, Error::NoMode { .. }), "{e}");
        let text = e.to_string();
        assert!(text.contains("expanded first"), "{text}");
        assert!(text.ends_with("use --mode raw"), "{text}");
    }

    fn fault(path: &Path) -> Error {
        Error::Image {
            path: path.display().to_string(),
            detail: "the CRC of a file entry is wrong".to_string(),
        }
    }

    #[test]
    fn a_file_that_cannot_be_read_names_the_way_past() {
        let file = std::env::temp_dir().join(format!("burnout-odd-{}.iso", std::process::id()));
        std::fs::write(&file, b"odd").unwrap();
        let e = check_mode(Err(fault(&file)), &file).unwrap_err();
        std::fs::remove_file(&file).unwrap();
        let text = e.to_string();
        assert!(
            text.contains(".iso: the CRC of a file entry is wrong."),
            "{text}"
        );
        assert!(text.ends_with("use --mode raw"), "{text}");
    }

    #[test]
    fn a_path_that_is_not_a_file_gives_its_fault_alone() {
        for path in [std::env::temp_dir(), "no-such-image.iso".into()] {
            let text = check_mode(Err(fault(&path)), &path)
                .unwrap_err()
                .to_string();
            assert!(!text.contains("--mode raw"), "{text}");
        }
    }

    #[test]
    fn macos_media_stays_refused_with_its_own_message() {
        let media = Error::MacosMedia {
            path: "Install.dmg".to_string(),
            what: "a compressed disk image of macOS",
        };
        let e = check_mode(Err(media), Path::new("Install.dmg")).unwrap_err();
        assert!(matches!(e, Error::MacosMedia { .. }), "{e}");
    }

    #[test]
    fn a_macos_drive_answers_to_its_node_its_name_and_the_name_under_dev() {
        let stick = DriveInfo::new(DriveId::new("disk5"), "/dev/rdisk5", "Flash Drive");
        for path in ["/dev/rdisk5", "disk5", "/dev/disk5"] {
            assert!(names_drive(&stick, path), "{path}");
        }
        for path in ["/dev/disk4", "/dev/rdisk", "disk", "/dev/disk5s1", "rdisk5"] {
            assert!(!names_drive(&stick, path), "{path}");
        }
    }

    #[test]
    fn a_linux_drive_answers_to_its_node_and_its_name() {
        let stick = DriveInfo::new(DriveId::new("sdb"), "/dev/sdb", "Flash Drive");
        assert!(names_drive(&stick, "/dev/sdb"));
        assert!(names_drive(&stick, "sdb"));
        assert!(!names_drive(&stick, "/dev/sdb1"));
    }
}
