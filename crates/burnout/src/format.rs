//! How the list command prints a drive.
//!
//! This module makes no system call and reads no drive. It takes the drives
//! and returns the lines, so a test checks the table on any host.

use burnout_core::{size_column, DriveInfo};

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
