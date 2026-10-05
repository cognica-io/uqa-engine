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
