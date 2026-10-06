//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::TemporalTimeZone;

fn seconds(year: i32, month: u8, day: u8) -> i64 {
    tz::UtcDateTime::new(year, month, day, 0, 0, 0, 0)
        .unwrap()
        .unix_time()
}

#[test]
fn bundled_names_keep_case_aliases_and_exact_punctuation() {
    assert_eq!(
        TemporalTimeZone::canonical_name("aMeRiCa/NeW_YoRk"),
        Some("America/New_York")
    );
    assert_eq!(
        TemporalTimeZone::canonical_name("US/Eastern"),
        Some("US/Eastern")
    );
    assert_eq!(
        TemporalTimeZone::canonical_name(":Asia/Seoul"),
        Some("Asia/Seoul")
    );
    let unix = seconds(2024, 7, 1);
    for name in ["America/New_York", "america/new_york", ":US/Eastern"] {
        assert_eq!(
            TemporalTimeZone::named(name).unwrap().offset_at(unix),
            Some(-14_400)
        );
    }
    for name in [
        "America/New\u{7f}York",
        " UTC",
        "UTC ",
        "UTC\0",
        "Not/A_Zone",
        "",
    ] {
        assert!(TemporalTimeZone::named(name).is_none(), "{name:?}");
    }
}

#[test]
fn sql_abbreviations_precede_names_but_session_names_do_not() {
    let summer = seconds(2024, 7, 1);
    assert_eq!(
        TemporalTimeZone::named("CET").unwrap().offset_at(summer),
        Some(7_200)
    );
    assert_eq!(
        TemporalTimeZone::named_or_abbreviation("cEt")
            .unwrap()
            .offset_at(summer),
        Some(3_600)
    );
    assert_eq!(
        TemporalTimeZone::named_or_abbreviation("IST")
            .unwrap()
            .offset_at(summer),
        Some(7_200)
    );
    assert_eq!(
        TemporalTimeZone::named_or_abbreviation("PDT")
            .unwrap()
            .offset_at(summer),
        Some(-25_200)
    );
    assert_eq!(
        TemporalTimeZone::named_or_abbreviation("MSK")
            .unwrap()
            .offset_at(seconds(2012, 7, 1)),
        Some(14_400)
    );
    assert_eq!(
        TemporalTimeZone::named_or_abbreviation("MSK")
            .unwrap()
            .offset_at(summer),
        Some(10_800)
    );
}

#[test]
fn local_gap_and_fold_choose_postgresql_offsets_without_dst_labels() {
    // Independently selected by PostgreSQL 18.4 AT TIME ZONE. In Dublin winter
    // has negative DST; the selected side still depends on the transition direction.
    for (name, local, offset) in [
        ("America/New_York", 1_710_037_800, -18_000),
        ("America/New_York", 1_730_597_400, -18_000),
        ("Europe/Dublin", 1_729_992_600, 0),
        ("Europe/Dublin", 1_711_848_600, 0),
        ("Pacific/Apia", 1_325_246_400, -36_000),
        ("Australia/Lord_Howe", 1_712_454_300, 37_800),
        ("Australia/Lord_Howe", 1_728_180_900, 37_800),
    ] {
        assert_eq!(
            TemporalTimeZone::named(name)
                .unwrap()
                .offset_for_local(local),
            Some(offset),
            "{name} at {local}"
        );
    }
    let zone = TemporalTimeZone::named("America/New_York").unwrap();
    assert_eq!(zone.offset_at(1_730_611_800), Some(-14_400));
    assert_eq!(zone.offset_at(1_730_615_400), Some(-18_000));
}

#[test]
fn historical_seconds_and_dates_beyond_four_digit_years_are_preserved() {
    for (name, past, future) in [
        ("Europe/Amsterdam", 1_172, 7_200),
        ("Asia/Seoul", 30_472, 32_400),
        ("America/New_York", -18_000, -14_400),
        ("Africa/Accra", -52, 0),
    ] {
        let zone = TemporalTimeZone::named(name).unwrap();
        assert_eq!(zone.offset_at(seconds(1900, 1, 15)), Some(past));
        assert_eq!(zone.offset_at(seconds(12_000, 7, 15)), Some(future));
    }
    let seoul = TemporalTimeZone::named("Asia/Seoul").unwrap();
    assert_eq!(seoul.offset_at(-210_866_803_200), Some(30_472));
    assert!(seoul.offset_at(i64::MAX / 1_000_000).is_some());
    assert!(seoul.offset_for_local(i64::MAX / 1_000_000).is_some());
}

#[test]
fn posix_offsets_rules_and_extensions_match_postgresql() {
    let summer = seconds(2024, 7, 1);
    for (name, offset) in [
        ("9", -32_400),
        ("+09:00", -32_400),
        ("UTC-09:30", 34_200),
        ("<+0530>-5:30", 19_800),
        ("UTC+167:59:60", -604_800),
        ("UTC-167:59:60", 604_800),
        ("AAA0BBB", 3_600),
        ("aaa0bbb,m3.2.0,m11.1.0", 3_600),
        ("AAA0BBB-1;M3.2.0,M11.1.0", 3_600),
        ("AAA-30BBB-31,M3.2.0,M11.1.0", 111_600),
        ("AAA0BBB,M3.2.0/26,M11.1.0/-2", 3_600),
    ] {
        assert_eq!(
            TemporalTimeZone::named(name).unwrap().offset_at(summer),
            Some(offset),
            "{name}"
        );
    }
    let zone = TemporalTimeZone::named("AAA0BBB,M3.2.0,M11.1.0").unwrap();
    assert_eq!(zone.offset_at(-62_150_241_600), Some(3_600));
    assert_eq!(zone.offset_at(316_533_182_400), Some(3_600));
    let leap_day = 1_709_208_000;
    assert_eq!(
        TemporalTimeZone::named("AAA0BBB,J60/0,J300/0")
            .unwrap()
            .offset_at(leap_day),
        Some(0)
    );
    assert_eq!(
        TemporalTimeZone::named("AAA0BBB,59/0,300/0")
            .unwrap()
            .offset_at(leap_day),
        Some(3_600)
    );
    let eastern = TemporalTimeZone::named("XXX5YYY,M3.2.0,M11.1.0").unwrap();
    assert_eq!(eastern.offset_for_local(1_710_037_800), Some(-18_000));
    assert_eq!(eastern.offset_for_local(1_730_597_400), Some(-18_000));
}

#[test]
fn invalid_posix_fields_do_not_fall_back_to_utc() {
    for name in [
        "UTC+168",
        "UTC+1:60",
        "UTC+1:00:61",
        "AAA0BBB,M0.1.0,M11.1.0",
        "AAA0BBB,M3.6.0,M11.1.0",
        "AAA0BBB,M3.2.7,M11.1.0",
        "AAA0BBB,J0,J365",
        "AAA0BBB,366,300",
        "AAA0BBB,M3.2.0/168,M11.1.0",
        "AAA0BBB,M3.2.0,M11.1.0/trailing",
        "<UTC0",
        ":UTC+1",
    ] {
        assert!(TemporalTimeZone::named(name).is_none(), "{name}");
    }
    assert!(TemporalTimeZone::fixed(i32::MIN).is_none());
    assert_eq!(
        TemporalTimeZone::fixed(30_472).unwrap().offset_at(i64::MIN),
        Some(30_472)
    );
}

#[test]
fn name_rule_and_local_resolution_do_not_allocate() {
    let measured = allocation_counter::measure(|| {
        for name in ["Europe/Amsterdam", "EST5EDT,M3.2.0,M11.1.0", "UTC-5:30"] {
            let zone = TemporalTimeZone::named(name).unwrap();
            assert!(zone.offset_at(1_710_037_800).is_some());
            assert!(zone.offset_for_local(1_710_037_800).is_some());
        }
        assert!(TemporalTimeZone::named_or_abbreviation("MSK").is_some());
        assert!(TemporalTimeZone::canonical_name("asia/seoul").is_some());
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}
