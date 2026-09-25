//! What Burnout refuses to write to, and what it asks before it writes.
//!
//! Every function here is pure. It takes a drive description and gives an
//! answer, and it opens nothing. That is what lets the rules be tested on
//! three hosts with no device anywhere near the test.
//!
//! `SECURITY.md` names a write to the wrong device as the worst thing this
//! project can do, so these rules come before the write path and not inside
//! it.

use crate::{grouped, DriveInfo, Error, Result};

/// Whether the person passed `--force`.
///
/// This is a type and not a `bool`, because a call that reads
/// `check_target(&drive, true)` says nothing about what the `true` means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Force {
    /// The person did not pass `--force`.
    No,
    /// The person passed `--force` and typed the drive back.
    Yes,
}

/// Refuse a drive that Burnout must not write to.
///
/// Three rules, and they are not the same rule.
///
/// The system disk is refused always. `--force` does not reach it, and
/// `CONTRIBUTING.md` says no flag allows it.
///
/// A drive that the host says refuses each write is refused always too, and
/// before the privilege. A password does not unlock a lock switch, and on
/// macOS the refusal of such a drive looks like a refusal of permission.
///
/// A drive that Burnout cannot prove is removable is refused unless the
/// person forced it, because an internal disk full of somebody's work looks
/// exactly like a large USB disk from here.
pub fn check_target(drive: &DriveInfo, force: Force) -> Result<()> {
    if drive.system {
        return Err(Error::SystemDisk {
            name: drive.name.clone(),
        });
    }
    if drive.read_only {
        return Err(Error::ReadOnly {
            name: drive.name.clone(),
        });
    }
    if !drive.removable() && force == Force::No {
        return Err(Error::NotRemovable {
            name: drive.name.clone(),
        });
    }
    Ok(())
}

/// Refuse an image that does not fit.
///
/// This runs before the write and not during it, so a drive that is too small
/// keeps whatever it held.
pub fn check_fits(image_bytes: u64, drive_bytes: u64) -> Result<()> {
    if image_bytes > drive_bytes {
        return Err(Error::TooSmall {
            image_bytes,
            drive_bytes,
        });
    }
    Ok(())
}

/// Refuse a drive that is not the one the person saw.
///
/// A number names a position in the list, and a position is not a drive. Pull
/// one drive out between `list` and `write`, and every later number moves.
/// So the write lists again and compares what it finds against what the
/// person read.
///
/// The comparison uses the identity that the host gives, the size, and the
/// model. A drive that matches all three is the drive.
pub fn check_same_drive(wanted: &DriveInfo, found: &DriveInfo) -> Result<()> {
    let same = wanted.id == found.id
        && wanted.size_bytes == found.size_bytes
        && wanted.model == found.model
        && wanted.serial == found.serial;
    if same {
        return Ok(());
    }
    Err(Error::DriveChanged {
        wanted: describe(wanted),
        found: describe(found),
    })
}

/// One line that names a drive to a person.
///
/// This is what a message shows when it has to tell two drives apart, so it
/// carries the size as well as the name.
pub fn describe(drive: &DriveInfo) -> String {
    format!(
        "{} ({}, {} bytes)",
        drive.name,
        drive.id.as_str(),
        grouped(drive.size_bytes)
    )
}

/// The words that `--force` asks a person to type.
///
/// `CONTRIBUTING.md` refuses a prompt that one keystroke answers, because
/// people learn to answer those without reading. This asks for the model and
/// the size of the drive, which a person can only produce by looking at the
/// drive in front of them.
pub fn force_phrase(drive: &DriveInfo) -> String {
    let model = drive
        .model
        .as_deref()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or(drive.name.as_str());
    format!("{} {}", model.trim(), drive.size_bytes)
}

/// Whether what the person typed is the phrase that [`force_phrase`] asked
/// for.
///
/// Case does not matter and neither does the spacing, because a person copies
/// this off a screen and a tab is not a mistake. Nothing else is forgiven: a
/// wrong number is a wrong drive.
pub fn phrase_matches(typed: &str, drive: &DriveInfo) -> bool {
    normalise(typed) == normalise(&force_phrase(drive))
}

/// Trim the ends, fold the case, and make every run of spaces one space.
fn normalise(text: &str) -> String {
    text.split_whitespace()
        .map(|word| word.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bus, Connection, DriveId};

    fn drive() -> DriveInfo {
        let mut d = DriveInfo::new(DriveId::new("disk4"), "/dev/rdisk4", "Samsung PSSD T7");
        d.model = Some("Samsung PSSD T7".to_string());
        d.serial = Some("S5TCNG0T123456".to_string());
        d.size_bytes = 1_000_204_886_016;
        d.bus = Bus::Usb;
        d.connection = Connection::External;
        d.removable_media = false;
        d
    }

    #[test]
    fn the_system_disk_is_refused_even_when_it_is_forced() {
        // This is the one rule with no way around it. A test that only
        // checks the unforced case would not notice a --force that reaches
        // this far.
        let mut d = drive();
        d.system = true;
        assert!(matches!(
            check_target(&d, Force::No),
            Err(Error::SystemDisk { .. })
        ));
        assert!(matches!(
            check_target(&d, Force::Yes),
            Err(Error::SystemDisk { .. })
        ));
    }

    #[test]
    fn a_read_only_drive_is_refused_even_when_it_is_forced() {
        let mut d = drive();
        d.read_only = true;
        for force in [Force::No, Force::Yes] {
            assert!(
                matches!(check_target(&d, force), Err(Error::ReadOnly { .. })),
                "{force:?}"
            );
        }
    }

    #[test]
    fn a_usb_drive_that_reports_fixed_media_is_still_a_target() {
        // A USB solid state disk says removable_media false on Linux and on
        // Windows. The connection is what makes it a target.
        let d = drive();
        assert!(!d.removable_media);
        assert!(d.removable());
        assert!(check_target(&d, Force::No).is_ok());
    }

    #[test]
    fn an_internal_drive_needs_the_force_option() {
        let mut d = drive();
        d.bus = Bus::Nvme;
        d.connection = Connection::Internal;
        assert!(matches!(
            check_target(&d, Force::No),
            Err(Error::NotRemovable { .. })
        ));
        assert!(check_target(&d, Force::Yes).is_ok());
    }

    #[test]
    fn an_image_the_size_of_the_drive_fits() {
        assert!(check_fits(1000, 1000).is_ok());
        assert!(check_fits(999, 1000).is_ok());
        assert!(matches!(
            check_fits(1001, 1000),
            Err(Error::TooSmall { .. })
        ));
    }

    #[test]
    fn the_same_drive_passes_the_identity_check() {
        assert!(check_same_drive(&drive(), &drive()).is_ok());
    }

    #[test]
    fn a_drive_of_the_same_name_and_a_different_size_is_a_different_drive() {
        // Two sticks unplugged and plugged back in the other order keep the
        // kernel names and swap the drives.
        let mut other = drive();
        other.size_bytes = 32_000_000_000;
        assert!(matches!(
            check_same_drive(&drive(), &other),
            Err(Error::DriveChanged { .. })
        ));
    }

    #[test]
    fn a_drive_of_the_same_size_and_a_different_serial_is_a_different_drive() {
        let mut other = drive();
        other.serial = Some("OTHER".to_string());
        assert!(check_same_drive(&drive(), &other).is_err());
    }

    #[test]
    fn the_force_phrase_holds_the_model_and_the_exact_size() {
        assert_eq!(force_phrase(&drive()), "Samsung PSSD T7 1000204886016");
    }

    #[test]
    fn the_force_phrase_falls_back_to_the_name_when_there_is_no_model() {
        let mut d = drive();
        d.model = None;
        assert_eq!(force_phrase(&d), "Samsung PSSD T7 1000204886016");
        d.model = Some("   ".to_string());
        assert_eq!(force_phrase(&d), "Samsung PSSD T7 1000204886016");
    }

    #[test]
    fn the_phrase_forgives_the_case_and_the_spacing() {
        let d = drive();
        assert!(phrase_matches("samsung pssd t7 1000204886016", &d));
        assert!(phrase_matches("  Samsung   PSSD\tT7  1000204886016 ", &d));
    }

    #[test]
    fn the_phrase_does_not_forgive_a_wrong_size() {
        // A wrong number is a different drive, and this is the last thing
        // between a person and a disk they did not mean to erase.
        let d = drive();
        assert!(!phrase_matches("Samsung PSSD T7 1000204886015", &d));
        assert!(!phrase_matches("Samsung PSSD T7", &d));
        assert!(!phrase_matches("yes", &d));
        assert!(!phrase_matches("", &d));
    }

    #[test]
    fn a_description_carries_the_size_so_that_two_drives_tell_apart() {
        assert_eq!(
            describe(&drive()),
            "Samsung PSSD T7 (disk4, 1,000,204,886,016 bytes)"
        );
    }
}
