//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

/// 2024-03-15 13:45:30.5 UTC as Unix microseconds, the transaction start the special values resolve against.
const NOW: i64 = 1_710_510_330_500_000;
const DAY: i64 = 86_400_000_000;

fn date(days: i32) -> TemporalValue {
    TemporalValue::Date { days }
}

#[test]
fn iso_dates_read_in_every_accepted_spelling() {
    let expected = date(19_724);
    for text in [
        "2024-01-02",
        " 2024-01-02 ",
        "2024/01/02",
        "2024.01.02",
        "20240102",
        "240102",
        "01-02-2024",
        "1-2-24",
        "2024-01-02 10:00",
        "2024-01-02T10:00:00Z",
        "2024-01-02, 10:00",
    ] {
        assert_eq!(
            TemporalValue::date_input(text, NOW),
            Ok(expected.clone()),
            "{text}"
        );
    }
}

#[test]
fn date_fields_report_the_overflow_postgresql_reports() {
    for (text, date_style) in [
        ("2024-13-01", true),
        ("2024-00-10", true),
        ("2024-01-32", true),
        ("24-01-01", true),
        ("2024-02-30", false),
        ("2023-02-29", false),
    ] {
        assert_eq!(
            TemporalValue::date_input(text, NOW),
            Err(TemporalInputError::FieldOverflow { date_style }),
            "{text}"
        );
    }
    assert_eq!(
        TemporalValue::date_input("2024-02-29", NOW),
        Ok(date(19_782))
    );
    for text in [
        "",
        "   ",
        "x",
        "10:00:00",
        "1 day",
        "nowx",
        "allballs",
        "Jan 1 2024",
    ] {
        assert_eq!(
            TemporalValue::date_input(text, NOW),
            Err(TemporalInputError::InvalidSyntax),
            "{text}"
        );
    }
    assert_eq!(
        TemporalValue::date_input("5874898-01-01", NOW),
        Err(TemporalInputError::OutOfRange)
    );
}

#[test]
fn special_dates_resolve_against_the_transaction_start() {
    let today = i32::try_from(NOW.div_euclid(DAY)).unwrap();
    assert_eq!(TemporalValue::date_input("now", NOW), Ok(date(today)));
    assert_eq!(TemporalValue::date_input(" TODAY ", NOW), Ok(date(today)));
    assert_eq!(
        TemporalValue::date_input("tomorrow", NOW),
        Ok(date(today + 1))
    );
    assert_eq!(
        TemporalValue::date_input("yesterday", NOW),
        Ok(date(today - 1))
    );
    assert_eq!(TemporalValue::date_input("epoch", NOW), Ok(date(0)));
    assert_eq!(
        TemporalValue::timestamp_input("now", NOW),
        Ok(TemporalValue::Timestamp { micros: NOW })
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("now", NOW),
        Ok(TemporalValue::TimestampTz { micros: NOW })
    );
    assert_eq!(
        TemporalValue::timestamp_input("today 10:00", NOW),
        Ok(TemporalValue::Timestamp {
            micros: i64::from(today) * DAY + 10 * 3_600_000_000
        })
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("tomorrow 10:00+02", NOW),
        Ok(TemporalValue::TimestampTz {
            micros: i64::from(today + 1) * DAY + 8 * 3_600_000_000
        })
    );
    assert_eq!(
        TemporalValue::timestamp_input("epoch", NOW),
        Ok(TemporalValue::Timestamp { micros: 0 })
    );
    for text in ["now 10:00", "epoch 10:00", "allballs"] {
        assert_eq!(
            TemporalValue::timestamp_input(text, NOW),
            Err(TemporalInputError::InvalidSyntax),
            "{text}"
        );
    }
}

#[test]
fn times_read_fractions_meridians_and_the_sixtieth_second() {
    let time = |micros| Ok(TemporalValue::Time { micros });
    assert_eq!(
        TemporalValue::time_input("10:00", NOW),
        time(36_000_000_000)
    );
    assert_eq!(
        TemporalValue::time_input("10:00:60", NOW),
        time(36_060_000_000)
    );
    assert_eq!(TemporalValue::time_input("23:59:60", NOW), time(DAY));
    assert_eq!(TemporalValue::time_input("24:00:00", NOW), time(DAY));
    assert_eq!(
        TemporalValue::time_input("10:00:00.9999995", NOW),
        time(36_001_000_000)
    );
    assert_eq!(
        TemporalValue::time_input("10:00:00.1234567", NOW),
        time(36_000_123_457)
    );
    assert_eq!(
        TemporalValue::time_input("10:00 PM", NOW),
        time(79_200_000_000)
    );
    assert_eq!(TemporalValue::time_input("12:00 am", NOW), time(0));
    assert_eq!(
        TemporalValue::time_input("12:30 pm", NOW),
        time(45_000_000_000)
    );
    assert_eq!(
        TemporalValue::time_input("2024-01-01 10:00", NOW),
        time(36_000_000_000)
    );
    assert_eq!(
        TemporalValue::time_input("10:00+05", NOW),
        time(36_000_000_000)
    );
    assert_eq!(
        TemporalValue::time_input("now", NOW),
        time(NOW.rem_euclid(DAY))
    );
    assert_eq!(TemporalValue::time_input("allballs", NOW), time(0));
    for text in ["25:00", "10:60", "10:00:61", "24:00:01", "13:00 pm"] {
        assert_eq!(
            TemporalValue::time_input(text, NOW),
            Err(TemporalInputError::FieldOverflow { date_style: false }),
            "{text}"
        );
    }
    for text in [
        "x",
        "epoch",
        "today",
        "yesterday",
        "2024-01-01",
        "10:00:00 x",
    ] {
        assert_eq!(
            TemporalValue::time_input(text, NOW),
            Err(TemporalInputError::InvalidSyntax),
            "{text}"
        );
    }
}

#[test]
fn zones_apply_to_instants_and_report_their_own_errors() {
    let instant = |micros| Ok(TemporalValue::TimestampTz { micros });
    let base = 19_723 * DAY + 10 * 3_600_000_000;
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00", NOW),
        instant(base)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00Z", NOW),
        instant(base)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00 utc", NOW),
        instant(base)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01T10:00:00z", NOW),
        instant(base)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00+05", NOW),
        instant(base - 5 * 3_600_000_000)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00 +05", NOW),
        instant(base - 5 * 3_600_000_000)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00+0530", NOW),
        instant(base - (5 * 3_600 + 30 * 60) * 1_000_000)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00+05:30:15", NOW),
        instant(base - (5 * 3_600 + 30 * 60 + 15) * 1_000_000)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00-05:30", NOW),
        instant(base + (5 * 3_600 + 30 * 60) * 1_000_000)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00+15:59", NOW),
        instant(base - (15 * 3_600 + 59 * 60) * 1_000_000)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00+16", NOW),
        Err(TemporalInputError::ZoneDisplacement)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00+05:60", NOW),
        Err(TemporalInputError::ZoneDisplacement)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00 Nowhere/Zone", NOW),
        Err(TemporalInputError::UnknownZone("nowhere/zone".into()))
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 10:00:00+05 x", NOW),
        Err(TemporalInputError::InvalidSyntax)
    );
}

#[test]
fn date_only_timestamps_read_separate_and_adjacent_numeric_zone_fields() {
    let base = 19_723 * DAY;
    // PostgreSQL 18.4 DecodeTimezone reads unseparated digits as HHMM, never HHMMSS.
    for (text, seconds_east) in [
        ("2024-01-01+00", 0),
        ("2024-01-01Z", 0),
        ("2024-01-01z", 0),
        ("2024-01-01+5", 18_000),
        ("2024-01-01+050", 3_000),
        ("2024-01-01+000001", 60),
        ("2024-01-01+0530", 19_800),
        ("2024-01-01+05:30", 19_800),
        ("2024-01-01+05:30:15", 19_815),
        ("2024-01-01+05:", 18_000),
        ("2024-01-01+05:30:", 19_800),
        ("2024-01-01 +05:30", 19_800),
        ("2024-01-01 -05:30:15", -19_815),
        ("20240101+05:30", 19_800),
        ("2024/01/01+05:30", 19_800),
        ("2024.01.01-05:30", -19_800),
        ("2024-01-01+15:59:59", 57_599),
    ] {
        assert_eq!(
            TemporalValue::timestamp_tz_input(text, NOW),
            Ok(TemporalValue::TimestampTz {
                micros: base - seconds_east * 1_000_000
            }),
            "{text}"
        );
        assert_eq!(
            TemporalValue::timestamp_input(text, NOW),
            Ok(TemporalValue::Timestamp { micros: base }),
            "{text}"
        );
        assert_eq!(
            TemporalValue::date_input(text, NOW),
            Ok(date(19_723)),
            "{text}"
        );
    }
    assert_eq!(
        TemporalValue::time_input("2024-01-01+05", NOW),
        Err(TemporalInputError::InvalidSyntax)
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-01-01 12:00 +05:30", NOW),
        Ok(TemporalValue::TimestampTz {
            micros: base + 23_400_000_000
        })
    );
}

#[test]
fn date_only_zone_tokens_keep_postgresql_syntax_and_displacement_diagnostics() {
    for text in [
        "2024-01-01-05",
        "2024-01-01-05:30:15",
        "0002-06-15-00:00:01 bc",
        "2024-01-01-15:59:59",
        "2024-01-01+05:30:1.5",
        "2024-01-01+",
        "2024-01-01+05+06",
        "2024-01-01+05 UTC",
        "2024-01-01-05:30 12:00",
    ] {
        assert_eq!(
            TemporalValue::timestamp_tz_input(text, NOW),
            Err(TemporalInputError::InvalidSyntax),
            "{text}"
        );
    }
    for text in [
        "2024-01-01+053015",
        "2024-01-01+16",
        "2024-01-01+05:60",
        "2024-01-01+05:30:60",
        "2024-13-01+16",
        "2024-02-30+16",
    ] {
        assert_eq!(
            TemporalValue::timestamp_tz_input(text, NOW),
            Err(TemporalInputError::ZoneDisplacement),
            "{text}"
        );
    }
}

#[test]
fn date_only_zone_inputs_apply_the_era_and_then_check_the_final_instant() {
    for (text, micros) in [
        ("0001-01-01+05:30 BC", -62_167_239_000_000_000),
        ("0002-06-15 -00:00:01 BC", -62_184_499_199_000_000),
        ("0001-02-29+00 BC", -62_162_121_600_000_000),
        ("4714-11-24+00 BC", -210_866_803_200_000_000),
    ] {
        assert_eq!(
            TemporalValue::timestamp_tz_input(text, NOW),
            Ok(TemporalValue::TimestampTz { micros }),
            "{text}"
        );
    }
    assert_eq!(
        TemporalValue::timestamp_tz_input("4714-11-24+00:00:01 BC", NOW),
        Err(TemporalInputError::OutOfRange)
    );
}

#[test]
fn timestamps_ignore_offsets_and_validate_their_fields() {
    let base = 19_723 * DAY + 10 * 3_600_000_000;
    // A `timestamp` reads and ignores the offset; a `timetz` keeps it.
    assert_eq!(
        TemporalValue::timestamp_input("2024-01-01 10:00:00+05", NOW),
        Ok(TemporalValue::Timestamp { micros: base })
    );
    assert_eq!(
        TemporalValue::time_tz_input("10:00+05:30", NOW),
        Ok(TemporalValue::TimeTz {
            micros: 36_000_000_000,
            offset_minutes: 330
        })
    );
    assert_eq!(
        TemporalValue::time_tz_input("allballs", NOW),
        Ok(TemporalValue::TimeTz {
            micros: 0,
            offset_minutes: 0
        })
    );
    assert_eq!(
        TemporalValue::timestamp_input("2024-01-01 25:00", NOW),
        Err(TemporalInputError::FieldOverflow { date_style: false })
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("2024-13-01 10:00", NOW),
        Err(TemporalInputError::FieldOverflow { date_style: true })
    );
    assert_eq!(
        TemporalValue::timestamp_input("2024-01-01 23:59:60", NOW),
        Ok(TemporalValue::Timestamp {
            micros: 19_724 * DAY
        })
    );
    assert_eq!(
        TemporalValue::timestamp_input("294277-01-01", NOW),
        Err(TemporalInputError::OutOfRange)
    );
    for text in ["x", "10:00:00", "1 day"] {
        assert_eq!(
            TemporalValue::timestamp_input(text, NOW),
            Err(TemporalInputError::InvalidSyntax),
            "{text}"
        );
    }
}

#[test]
fn intervals_keep_syntax_and_overflow_apart() {
    let interval = |months, days, micros| {
        Ok(TemporalValue::Interval {
            months,
            days,
            micros,
        })
    };
    let read = |text: &str| {
        TemporalValue::interval_input_with_control(
            text,
            &crate::memory::ProductionControl::uncontrolled(),
        )
        .unwrap()
    };
    assert_eq!(read("1 day 2"), interval(0, 1, 2_000_000));
    assert_eq!(read("1 day ago"), interval(0, -1, 0));
    assert_eq!(read("@ 1 day"), interval(0, 1, 0));
    assert_eq!(
        read("3 4:05:06.5"),
        interval(0, 3, (4 * 3_600 + 5 * 60 + 6) * 1_000_000 + 500_000)
    );
    assert_eq!(read("1 day 25:00:00"), interval(0, 1, 25 * 3_600_000_000));
    assert_eq!(read("10:00:60"), interval(0, 0, 36_060_000_000));
    assert_eq!(read("1 1:00:60"), interval(0, 1, 3_660_000_000));
    assert_eq!(read("-1-2"), interval(-14, 0, 0));
    assert_eq!(read("1 year -2 mons"), interval(10, 0, 0));
    assert_eq!(read("1.5 mons"), interval(1, 15, 0));
    for text in ["", "   ", "ago", "1 2", "1 dayx", "x day", "nan"] {
        assert_eq!(read(text), Err(TemporalInputError::InvalidSyntax), "{text}");
    }
    for text in [
        "1-13",
        "1 1:60:00",
        "1 1:00:61",
        "1:60",
        "99999999999 days",
        "1 day 99999999999999999999 seconds",
        "999999999999 years",
    ] {
        assert_eq!(
            read(text),
            Err(TemporalInputError::IntervalFieldOverflow),
            "{text}"
        );
    }
}

#[test]
fn eras_count_years_before_the_common_era() {
    let days = |year, month, day| {
        i32::try_from(
            NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .signed_duration_since(epoch_date())
                .num_days(),
        )
        .unwrap()
    };
    assert_eq!(
        TemporalValue::date_input("0001-01-01 BC", NOW),
        Ok(date(days(0, 1, 1)))
    );
    assert_eq!(
        TemporalValue::date_input("0002-06-15 bc", NOW),
        Ok(date(days(-1, 6, 15)))
    );
    assert_eq!(
        TemporalValue::date_input("2024-01-01 AD", NOW),
        Ok(date(19_723))
    );
    assert_eq!(
        TemporalValue::timestamp_tz_input("0002-06-15 10:00 BC", NOW),
        Ok(TemporalValue::TimestampTz {
            micros: i64::from(days(-1, 6, 15)) * DAY + 10 * 3_600_000_000
        })
    );
    for text in ["0000-01-01", "0000-01-01 BC"] {
        assert_eq!(
            TemporalValue::date_input(text, NOW),
            Err(TemporalInputError::FieldOverflow { date_style: false }),
            "{text}"
        );
    }
    for text in ["BC", "2024-01-01 BC AD", "2024-01-01 AD BC", "now BC"] {
        assert_eq!(
            TemporalValue::timestamp_input(text, NOW),
            Err(TemporalInputError::InvalidSyntax),
            "{text}"
        );
    }
}

#[test]
fn timestamp_input_checks_the_julian_boundary_after_the_zone_offset() {
    const MINIMUM: i64 = -210_866_803_200_000_000;
    assert_eq!(
        TemporalValue::timestamp_input("4714-11-24 BC", NOW),
        Ok(TemporalValue::Timestamp { micros: MINIMUM })
    );
    assert_eq!(
        TemporalValue::timestamp_input("4714-11-23 23:59:59.999999 BC", NOW),
        Err(TemporalInputError::OutOfRange)
    );
    for text in ["4714-11-24 00:00:00+00 BC", "4714-11-23 12:00:00-12 BC"] {
        assert_eq!(
            TemporalValue::timestamp_tz_input(text, NOW),
            Ok(TemporalValue::TimestampTz { micros: MINIMUM }),
            "{text}"
        );
    }
    for text in [
        "4714-11-24 00:00:00+00:00:01 BC",
        "4714-11-23 23:59:59.999999+00 BC",
    ] {
        assert_eq!(
            TemporalValue::timestamp_tz_input(text, NOW),
            Err(TemporalInputError::OutOfRange),
            "{text}"
        );
    }
    assert!(!TemporalValue::timestamp_micros_in_range(MINIMUM - 1));
    assert!(TemporalValue::timestamp_micros_in_range(MINIMUM));
    assert!(TemporalValue::timestamp_micros_in_range(i64::MAX));
}

#[test]
fn numeric_date_order_and_short_era_years_match_postgresql() {
    for (order, expected, era) in [
        (
            TemporalDateOrder::MonthDayYear,
            "2003-01-02",
            "0003-01-02 BC",
        ),
        (
            TemporalDateOrder::DayMonthYear,
            "2003-02-01",
            "0003-02-01 BC",
        ),
        (
            TemporalDateOrder::YearMonthDay,
            "2001-02-03",
            "0001-02-03 BC",
        ),
    ] {
        for separator in ['/', '-', '.'] {
            let source = format!("01{separator}02{separator}03");
            assert_eq!(
                TemporalValue::date_input_in_order(&source, NOW, order)
                    .unwrap()
                    .to_sql_string(),
                expected
            );
            assert_eq!(
                TemporalValue::date_input_in_order(&format!("{source} BC"), NOW, order)
                    .unwrap()
                    .to_sql_string(),
                era
            );
        }
        for source in ["2020-02-03", "20200203", "200203"] {
            assert_eq!(
                TemporalValue::date_input_in_order(source, NOW, order)
                    .unwrap()
                    .to_sql_string(),
                "2020-02-03"
            );
        }
    }
    assert_eq!(
        TemporalValue::date_input_in_order("00/02/03 BC", NOW, TemporalDateOrder::YearMonthDay),
        Err(TemporalInputError::FieldOverflow { date_style: false })
    );
    assert_eq!(
        TemporalValue::date_input_in_order("00/02/03", NOW, TemporalDateOrder::YearMonthDay)
            .unwrap()
            .to_sql_string(),
        "2000-02-03"
    );
}
