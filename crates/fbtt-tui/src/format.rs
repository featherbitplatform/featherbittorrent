//! Numbers and remote text in the form a person reads them.
//!
//! Rates and sizes are stated the way a person states them out loud: bytes below one
//! KiB, one decimal place above it, and the next unit up as soon as the number would
//! read as 1024.0 of this one. Everything is computed in whole tenths so that the same
//! byte count always produces the same string, which a table that redraws thirty times
//! a second needs: a percent that jitters between two values looks like a bug even when
//! it is arithmetic.
//!
//! [`safe`] is the other half of this file. Torrent names, file paths, peer and tracker
//! text all came from somewhere else, and every one of them gets here through it, so
//! that a name cannot reverse the row it sits in or run past the column drawn for it.

use fbt::{MAX_DISPLAY_CHARS, display_safe};

/// The units [`size`] and [`rate`] count in: binary, because that is what a person
/// comparing a file size against a disk expects.
const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
/// How many of the previous unit make the next one.
const UNIT_STEP: u128 = 1024;
/// A value this many tenths of a unit (1024.0) belongs in the unit above it.
const ROUNDED_UP: u128 = UNIT_STEP * 10;

/// `s` as a user should see it, cut to the length remote text is allowed to be: control
/// and format characters gone, whitespace collapsed, at most [`MAX_DISPLAY_CHARS`]
/// characters. Row strings arrive already cleaned but still checked here, because a
/// screen cannot know which backend produced them.
pub fn safe(s: &str) -> String {
    display_safe(s, MAX_DISPLAY_CHARS)
}

/// A transfer rate, as `4.2 MiB/s`.
pub fn rate(bytes_per_sec: u64) -> String {
    let mut text = size(bytes_per_sec);
    text.push_str("/s");
    text
}

/// A byte count, as `3.1 GiB`. Whole bytes below one KiB, one decimal from there up.
pub fn size(bytes: u64) -> String {
    let bytes = u128::from(bytes);

    // The largest unit the value reaches whole, so 999 KiB never reads as 0.9 MiB.
    let mut unit = 0usize;
    let mut divisor: u128 = 1;
    while unit + 1 < UNITS.len() && bytes >= divisor.saturating_mul(UNIT_STEP) {
        unit += 1;
        divisor *= UNIT_STEP;
    }

    // The value in tenths of that unit, rounded half up: a tenth is a digit, not a float.
    let mut tenths = (bytes * 10 + divisor / 2) / divisor;
    // A value that rounds up to a full 1024 of this unit is one of the next unit instead,
    // so no row ever has to explain that 1024.0 MiB means 1.0 GiB.
    if unit + 1 < UNITS.len() && tenths >= ROUNDED_UP {
        unit += 1;
        divisor *= UNIT_STEP;
        tenths = (bytes * 10 + divisor / 2) / divisor;
    }

    let whole = tenths / 10;
    let tenth = tenths % 10;
    if unit == 0 {
        format!("{whole} {}", UNITS[unit])
    } else {
        format!("{whole}.{tenth} {}", UNITS[unit])
    }
}

/// A fraction as a whole percent: `62%`. Values outside `0.0..=1.0` and NaN are clamped,
/// because a rate or a progress number comes from the engine and is not worth a broken row.
pub fn percent(progress: f64) -> String {
    format!("{}%", filled_percent(progress))
}

/// The fraction of `width` cells `progress` fills, rounded to the nearest cell.
pub fn filled_cells(progress: f64, width: u16) -> u16 {
    let filled = (f64::from(width) * clamp(progress)).round();
    if filled <= 0.0 {
        0
    } else if filled >= f64::from(width) {
        width
    } else {
        // `filled` is inside `0.0..width`, so it fits a u16 and is not negative.
        filled as u16
    }
}

/// The percent to show for a progress number, clamped to `0..=100`.
fn filled_percent(progress: f64) -> u8 {
    let scaled = (clamp(progress) * 100.0).round();
    if scaled <= 0.0 {
        0
    } else if scaled >= 100.0 {
        100
    } else {
        // `scaled` is inside `0.0..100`, so it fits a u8 and is not negative.
        scaled as u8
    }
}

/// `x` inside `0.0..=1.0`; NaN, which no progress number should be, becomes 0.
fn clamp(x: f64) -> f64 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_fractions_in_range() {
        assert_eq!(clamp(0.5), 0.5);
        assert_eq!(clamp(-2.0), 0.0);
        assert_eq!(clamp(3.0), 1.0);
        assert_eq!(clamp(f64::NAN), 0.0);
    }
}
