//! Sizes, as a person reads them.
//!
//! The list, the confirmation, the report and the error messages all print a
//! size, and they print it in one way.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
