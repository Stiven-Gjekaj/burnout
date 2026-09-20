//! The order that `burnout list` prints, and the number that a person types.
//!
//! A device path cannot be the same on three hosts. `/dev/disk4`, `/dev/sdb`
//! and `\\.\PhysicalDrive2` have nothing in common, so a person names a drive
//! by its number and never by its path.
//!
//! The number is the position in this order, and it is not a field on the
//! drive. A field can go stale. A position cannot, because it is a function
//! of the set of drives that the host reports.

use std::cmp::Ordering;

use crate::{DriveInfo, Result};

/// Every whole drive that the host reports.
///
/// An implementation opens no device and needs no privilege. A person sees
/// their drives before they give a password.
///
/// A drive that the host describes badly does not fail the whole list. The
/// implementation drops that drive and carries on, because a list that stops
/// at an empty card reader is a list that nobody can use.
pub trait DriveList {
    fn drives(&self) -> Result<Vec<DriveInfo>>;
}

/// Compare two host names the way a person counts.
///
/// A plain comparison of the bytes puts `disk10` before `disk9`, so a tenth
/// drive renumbers the list and the number that a person read a moment ago
/// now names a different drive.
///
/// A run of digits compares as a number. A run of letters compares by its
/// length first, so `sdz` comes before `sdaa`, which is the order that the
/// Linux kernel gives out the names.
pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let mut a = runs(left);
    let mut b = runs(right);
    loop {
        match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let order = match (
                    x.as_bytes()[0].is_ascii_digit(),
                    y.as_bytes()[0].is_ascii_digit(),
                ) {
                    (true, true) => digits(x)
                        .cmp(&digits(y))
                        .then_with(|| x.len().cmp(&y.len())),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => x.len().cmp(&y.len()).then_with(|| x.cmp(y)),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// Split a name into runs of digits and runs of everything else.
fn runs(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let digit = rest.as_bytes()[0].is_ascii_digit();
        let end = rest
            .char_indices()
            .find(|(_, c)| c.is_ascii_digit() != digit)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        rest = tail;
        Some(run)
    })
}

/// Read a run of digits as a number. A run too long for the number saturates,
/// which keeps the order deterministic rather than correct for a name that no
/// host produces.
fn digits(run: &str) -> u128 {
    run.parse().unwrap_or(u128::MAX)
}

/// Put the drives in the order that `list` prints.
///
/// The order comes from the name that the host gives the drive, and from
/// nothing that changes while the drive stays connected. The same set of
/// drives gives the same order every time.
pub fn in_list_order(mut drives: Vec<DriveInfo>) -> Vec<DriveInfo> {
    drives.sort_by(|a, b| natural_cmp(a.id.as_str(), b.id.as_str()));
    drives
}

/// Find the drive that a number names. The first drive is number 1.
///
/// Give this the list that [`in_list_order`] returned.
pub fn by_index(drives: &[DriveInfo], index: usize) -> Option<&DriveInfo> {
    if index == 0 {
        return None;
    }
    drives.get(index - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DriveId, DriveInfo};

    fn drive(id: &str) -> DriveInfo {
        DriveInfo::new(DriveId::new(id), format!("/dev/{id}"), "a drive")
    }

    fn order(ids: &[&str]) -> Vec<String> {
        let list = in_list_order(ids.iter().map(|i| drive(i)).collect());
        list.iter().map(|d| d.id.as_str().to_string()).collect()
    }

    #[test]
    fn a_tenth_disk_does_not_jump_in_front_of_the_ninth() {
        // A comparison of the bytes puts disk10 first, and then the number
        // that a person read a moment ago names a different drive.
        assert_eq!(
            order(&["disk10", "disk9", "disk1"]),
            ["disk1", "disk9", "disk10"]
        );
    }

    #[test]
    fn the_linux_names_keep_the_order_that_the_kernel_gives_them() {
        assert_eq!(
            order(&["sdaa", "sdb", "sdz", "sda"]),
            ["sda", "sdb", "sdz", "sdaa"]
        );
    }

    #[test]
    fn an_nvme_name_compares_by_each_number_in_it() {
        assert_eq!(
            order(&["nvme0n2", "nvme10n1", "nvme0n1", "nvme2n1"]),
            ["nvme0n1", "nvme0n2", "nvme2n1", "nvme10n1"]
        );
    }

    #[test]
    fn a_windows_number_compares_as_a_number() {
        assert_eq!(order(&["11", "2", "0"]), ["0", "2", "11"]);
    }

    #[test]
    fn the_order_does_not_depend_on_the_order_that_the_host_reported() {
        let one = order(&["disk2", "disk10", "disk1"]);
        let other = order(&["disk10", "disk1", "disk2"]);
        assert_eq!(one, other);
    }

    #[test]
    fn a_leading_zero_does_not_change_which_drive_is_first() {
        assert_eq!(order(&["disk007", "disk7"]), ["disk7", "disk007"]);
    }

    #[test]
    fn the_first_drive_is_number_one() {
        let list = in_list_order(vec![drive("sdb"), drive("sda")]);
        assert_eq!(by_index(&list, 1).unwrap().id.as_str(), "sda");
        assert_eq!(by_index(&list, 2).unwrap().id.as_str(), "sdb");
    }

    #[test]
    fn there_is_no_drive_number_zero() {
        let list = in_list_order(vec![drive("sda")]);
        assert!(by_index(&list, 0).is_none());
    }

    #[test]
    fn a_number_past_the_end_names_no_drive() {
        let list = in_list_order(vec![drive("sda")]);
        assert!(by_index(&list, 2).is_none());
    }

    #[test]
    fn an_empty_list_names_no_drive() {
        assert!(by_index(&[], 1).is_none());
    }

    struct FakeHost(Vec<DriveInfo>);

    impl DriveList for FakeHost {
        fn drives(&self) -> Result<Vec<DriveInfo>> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn a_list_of_drives_goes_through_the_order_and_then_the_number() {
        let host = FakeHost(vec![drive("disk10"), drive("disk2")]);
        let list = in_list_order(host.drives().unwrap());
        assert_eq!(by_index(&list, 1).unwrap().id.as_str(), "disk2");
    }

    #[test]
    fn a_host_with_no_drive_is_not_an_error() {
        let host = FakeHost(Vec::new());
        assert!(host.drives().unwrap().is_empty());
    }
}
