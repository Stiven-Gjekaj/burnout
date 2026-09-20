//! How the list command prints a drive.
//!
//! This module makes no system call and reads no drive. It takes the drives
//! and returns the lines, so a test checks the table on any host.

use burnout_core::DriveInfo;

/// A size for a person to read, in the units that the label on the drive uses.
///
/// Drives are sold in powers of ten, so a person compares against powers of
/// ten. A count in powers of two would make every drive look smaller than the
/// box it came in.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A byte count with a separator every three digits.
pub fn grouped(bytes: u64) -> String {
    let digits = bytes.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (count, ch) in digits.chars().rev().enumerate() {
        if count > 0 && count % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

/// The size column: a figure to read, and the exact count beside it.
///
/// Both, because the rounded figure is what a person recognises and the exact
/// count is the number that this code can prove.
pub fn size_column(bytes: u64) -> String {
    format!("{} ({} bytes)", human_size(bytes), grouped(bytes))
}

/// The whole table, as lines.
pub fn table(drives: &[DriveInfo]) -> Vec<String> {
    let mut rows: Vec<[String; 5]> = vec![[
        "#".to_string(),
        "DRIVE".to_string(),
        "SIZE".to_string(),
        "BUS".to_string(),
        "REMOVABLE".to_string(),
    ]];

    for (position, drive) in drives.iter().enumerate() {
        rows.push([
            (position + 1).to_string(),
            drive.name.clone(),
            size_column(drive.size_bytes),
            drive.bus.label().to_string(),
            if drive.removable() { "yes" } else { "no" }.to_string(),
        ]);
    }

    let mut width = [0usize; 5];
    for row in &rows {
        for (column, cell) in row.iter().enumerate() {
            width[column] = width[column].max(cell.chars().count());
        }
    }

    let mut lines = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let mut line = String::new();
        for (column, cell) in row.iter().enumerate() {
            if column > 0 {
                line.push_str("  ");
            }
            line.push_str(cell);
            // The last column takes no padding, so no line ends in spaces.
            if column + 1 < row.len() {
                for _ in cell.chars().count()..width[column] {
                    line.push(' ');
                }
            }
        }
        // The mark goes after the row, where it reads as a warning and not as
        // a column that somebody has to look up.
        if index > 0 && drives[index - 1].system {
            line.push_str("   (system disk)");
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use burnout_core::{Bus, Connection, DriveId, DriveInfo};

    fn drive(name: &str, size: u64, bus: Bus, system: bool) -> DriveInfo {
        let mut d = DriveInfo::new(DriveId::new(name), format!("/dev/{name}"), name);
        d.size_bytes = size;
        d.bus = bus;
        d.system = system;
        d.connection = if bus == Bus::Usb {
            Connection::External
        } else {
            Connection::Internal
        };
        d
    }

    #[test]
    fn a_size_uses_the_units_that_the_box_uses() {
        // A drive sold as one terabyte holds this many bytes.
        assert_eq!(human_size(1_000_204_886_016), "1.0 TB");
        assert_eq!(human_size(32_010_928_128), "32.0 GB");
    }

    #[test]
    fn a_small_size_stays_in_bytes_and_is_not_rounded() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(999), "999 B");
    }

    #[test]
    fn the_digits_are_grouped_so_that_a_person_can_count_them() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(512), "512");
        assert_eq!(grouped(1024), "1,024");
        assert_eq!(grouped(1_000_204_886_016), "1,000,204,886,016");
    }

    #[test]
    fn the_size_column_holds_the_exact_count_as_well_as_the_rounded_one() {
        // The rounded figure is what a person recognises. The exact count is
        // the number that this code can prove.
        assert_eq!(
            size_column(1_000_204_886_016),
            "1.0 TB (1,000,204,886,016 bytes)"
        );
    }

    #[test]
    fn the_table_numbers_the_drives_from_one() {
        let rows = table(&[
            drive("sda", 1_000_204_886_016, Bus::Nvme, true),
            drive("sdb", 32_010_928_128, Bus::Usb, false),
        ]);
        assert!(rows[1].starts_with('1'));
        assert!(rows[2].starts_with('2'));
    }

    #[test]
    fn the_system_disk_is_marked_where_a_person_reads_it() {
        let rows = table(&[drive("sda", 1024, Bus::Nvme, true)]);
        assert!(rows[1].contains("(system disk)"), "{}", rows[1]);
    }

    #[test]
    fn a_drive_that_is_not_the_system_disk_carries_no_mark() {
        let rows = table(&[drive("sdb", 1024, Bus::Usb, false)]);
        assert!(!rows[1].contains("system disk"));
    }

    #[test]
    fn the_columns_line_up_whatever_the_names_are() {
        let rows = table(&[
            drive("a", 1024, Bus::Usb, false),
            drive("a much longer drive name", 1024, Bus::Usb, false),
        ]);
        let bus_at: Vec<usize> = rows.iter().map(|r| r.find("USB").unwrap_or(0)).collect();
        assert_eq!(bus_at[1], bus_at[2], "the bus column moved between rows");
    }

    #[test]
    fn no_line_ends_in_a_space() {
        let rows = table(&[drive("sdb", 1024, Bus::Usb, false)]);
        for row in &rows {
            assert_eq!(row.trim_end(), row, "{row:?} ends in a space");
        }
    }

    #[test]
    fn a_table_of_no_drives_still_has_its_heading() {
        let rows = table(&[]);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].contains("DRIVE"));
    }
}
