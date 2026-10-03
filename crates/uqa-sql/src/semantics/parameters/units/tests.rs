//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{parse_integer, show_integer, ParameterUnit};

const TIME_HINT: &str =
    "Valid units for this parameter are \"us\", \"ms\", \"s\", \"min\", \"h\", and \"d\".";
const MEMORY_HINT: &str =
    "Valid units for this parameter are \"B\", \"kB\", \"MB\", \"GB\", and \"TB\".";
const RANGE_HINT: &str = "Value exceeds integer range.";

fn milliseconds(raw: &str) -> Result<i32, Option<&'static str>> {
    parse_integer(raw, Some(ParameterUnit::Milliseconds))
}

fn kilobytes(raw: &str) -> Result<i32, Option<&'static str>> {
    parse_integer(raw, Some(ParameterUnit::Kilobytes))
}

#[test]
fn time_values_read_units_and_round_as_postgres_does() {
    for (raw, expected) in [
        ("1s", 1000),
        ("1.5s", 1500),
        ("1500", 1500),
        ("60000", 60_000),
        ("2h", 7_200_000),
        ("1d", 86_400_000),
        ("1.5", 2),
        ("2.5", 2),
        ("1500us", 2),
        ("100us", 0),
        ("1 s", 1000),
        (" 1s ", 1000),
        ("1e3", 1000),
        ("010", 8),
        ("0x10", 16),
        ("-0", 0),
        ("+5", 5),
        (".5", 0),
        ("1.0005s", 1000),
    ] {
        assert_eq!(milliseconds(raw), Ok(expected), "{raw:?}");
    }
}

#[test]
fn time_values_reject_what_parse_int_rejects_with_its_hints() {
    for (raw, hint) in [
        ("abc", None),
        ("", None),
        ("inf", None),
        ("1e400", None),
        ("1 xyz", Some(TIME_HINT)),
        ("1S", Some(TIME_HINT)),
        ("08", Some(TIME_HINT)),
        ("1 s s", Some(TIME_HINT)),
        ("1mins", Some(TIME_HINT)),
        ("2147483648", Some(RANGE_HINT)),
        ("25d", Some(RANGE_HINT)),
        ("-2147483649", Some(RANGE_HINT)),
    ] {
        assert_eq!(milliseconds(raw), Err(hint), "{raw:?}");
    }
}

#[test]
fn memory_values_round_to_the_next_smaller_unit() {
    for (raw, expected) in [
        ("65536kB", 65536),
        ("65536", 65536),
        ("1GB", 1_048_576),
        ("1.5MB", 1536),
        ("1TB", 1_073_741_824),
        ("64B", 0),
        ("100000B", 98),
    ] {
        assert_eq!(kilobytes(raw), Ok(expected), "{raw:?}");
    }
    assert_eq!(kilobytes("1 KB"), Err(Some(MEMORY_HINT)));
    assert_eq!(kilobytes("2TB"), Err(Some(RANGE_HINT)));
}

#[test]
fn unitless_values_accept_no_unit() {
    assert_eq!(parse_integer("180000", None), Ok(180_000));
    assert_eq!(parse_integer("1s", None), Err(None));
    assert_eq!(parse_integer(" 7 ", None), Ok(7));
}

#[test]
fn values_show_in_the_greatest_unit_that_divides_them() {
    let unit = Some(ParameterUnit::Milliseconds);
    for (value, shown) in [
        (0, "0"),
        (-1, "-1"),
        (1000, "1s"),
        (1500, "1500ms"),
        (60_000, "1min"),
        (7_200_000, "2h"),
        (86_400_000, "1d"),
        (90_000, "90s"),
    ] {
        assert_eq!(show_integer(value, unit), shown);
    }
    let unit = Some(ParameterUnit::Kilobytes);
    for (value, shown) in [
        (65536, "64MB"),
        (4096, "4MB"),
        (1_048_576, "1GB"),
        (1000, "1000kB"),
        (1536, "1536kB"),
        (98, "98kB"),
    ] {
        assert_eq!(show_integer(value, unit), shown);
    }
    assert_eq!(show_integer(180_000, None), "180000");
}
