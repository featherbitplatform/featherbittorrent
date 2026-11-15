//! Rates and sizes the way a person reads them.

use fbtt_tui::format;

#[test]
fn rate_format() {
    let cases = [
        (0u64, "0 B/s"),
        (512, "512 B/s"),
        // A byte count below one KiB stays bytes: no unit nobody asked for.
        (1_023, "1023 B/s"),
        (1_024, "1.0 KiB/s"),
        (1_536, "1.5 KiB/s"),
        (317_440, "310.0 KiB/s"),
        (1_153_434, "1.1 MiB/s"),
        (4_404_019, "4.2 MiB/s"),
        // One byte short of the next unit still reads as that unit, not as 1024.0 of this one.
        (1_073_741_823, "1.0 GiB/s"),
        (1_099_511_627_776, "1.0 TiB/s"),
        (u64::MAX, "16.0 EiB/s"),
    ];
    for (bytes_per_sec, want) in cases {
        assert_eq!(
            format::rate(bytes_per_sec),
            want,
            "{bytes_per_sec} B/s should read as {want:?}"
        );
    }
}

#[test]
fn size_format() {
    let cases = [
        (0u64, "0 B"),
        (512, "512 B"),
        (1_024, "1.0 KiB"),
        (88_741, "86.7 KiB"),
        // The torrent size the design's mockup shows.
        (3_298_534_880, "3.1 GiB"),
        (1u64 << 40, "1.0 TiB"),
    ];
    for (bytes, want) in cases {
        assert_eq!(
            format::size(bytes),
            want,
            "{bytes} bytes should read as {want:?}"
        );
    }
}

#[test]
fn percent_and_bar_are_clamped_to_what_they_get() {
    assert_eq!(format::percent(0.62), "62%");
    assert_eq!(format::percent(1.0), "100%");
    assert_eq!(format::percent(0.0), "0%");
    // Numbers from a peer or an engine that are out of range cannot break the row.
    assert_eq!(format::percent(-0.5), "0%");
    assert_eq!(format::percent(2.0), "100%");
    assert_eq!(format::percent(f64::NAN), "0%");

    assert_eq!(format::filled_cells(0.62, 10), 6);
    assert_eq!(format::filled_cells(0.0, 10), 0);
    assert_eq!(format::filled_cells(1.0, 10), 10);
    assert_eq!(format::filled_cells(f64::NAN, 10), 0);
}

#[test]
fn safe_cuts_hostile_text_to_what_fits() {
    // A bidi override, a control character and a run of spaces are what remote text uses
    // to spoof a display; none of them survive. An escape sequence keeps the characters it
    // is spelled with once its escape is gone, so it can hide nothing and colour nothing.
    assert_eq!(format::safe("evil\u{202e}gpj.exe"), "evilgpj.exe");
    assert_eq!(format::safe("a\u{1b}[31mb\tc"), "a[31mb c");
    assert_eq!(format::safe("  padded   name  "), "padded name");

    let long = "x".repeat(10_000);
    let cut = format::safe(&long);
    assert_eq!(cut.chars().count(), fbt::MAX_DISPLAY_CHARS);
}
