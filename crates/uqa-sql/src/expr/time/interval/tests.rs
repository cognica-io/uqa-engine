//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::IntervalFields;

const fn fields(months: i32, days: i32, micros: i64) -> IntervalFields {
    IntervalFields {
        months,
        days,
        micros,
    }
}

fn sqlstate(result: crate::error::Result<IntervalFields>) -> String {
    result.unwrap_err().sqlstate().unwrap().to_string()
}

/// `interval_send` of `interval * float8` in `PostgreSQL` 18.
#[test]
fn multiplication_cascades_fractions_as_interval_mul_does() {
    let cases = [
        (fields(1, 0, 0), 0.5, fields(0, 15, 0)),
        (fields(0, 1, 0), 1.5, fields(0, 1, 43_200_000_000)),
        (fields(1, 1, 1_000_000), 0.3, fields(0, 9, 25_920_300_000)),
        (
            fields(7, 11, 47_839_123_457),
            1.1,
            fields(7, 33, 61_263_035_803),
        ),
        (
            fields(7, 11, 47_839_123_457),
            -2.7,
            fields(-18, -56, -189_645_633_334),
        ),
        (
            fields(-3, -5, 0),
            0.333_333,
            fields(0, -31, -57_597_264_000),
        ),
        (fields(1, 0, 0), 1e-7, fields(0, 0, 259_200)),
        (
            fields(0, 29, 86_399_999_999),
            1.000_000_1,
            fields(0, 29, 86_400_259_199),
        ),
        (
            fields(1200, 0, 0),
            1.234_567_89,
            fields(1481, 14, 38_365_056_000),
        ),
        (fields(0, 1, 0), 0.999_999_999_999, fields(0, 1, 0)),
        (
            fields(5, 0, 82_800_000_000),
            2.5,
            fields(12, 15, 207_000_000_000),
        ),
        (
            fields(0, 0, 1_000_000),
            1e10,
            fields(0, 0, 10_000_000_000_000_000),
        ),
        (fields(0, 0, 0), 1e300, fields(0, 0, 0)),
        (
            fields(17, -3, 14_400_000_000),
            -0.1,
            fields(-1, -21, 24_480_000_000),
        ),
    ];
    for (interval, factor, expected) in cases {
        assert_eq!(
            interval.multiply(factor).unwrap(),
            expected,
            "{interval:?} * {factor}"
        );
    }
}

/// `interval_send` of `interval / float8` in `PostgreSQL` 18, which divides each field instead of multiplying by the reciprocal.
#[test]
fn division_divides_each_field_as_interval_div_does() {
    let cases = [
        (fields(0, 1, 0), 3.0, fields(0, 0, 28_800_000_000)),
        (fields(1, 0, 0), 7.0, fields(0, 4, 24_685_689_600)),
        (fields(1, 1, 1_000_000), 0.3, fields(3, 13, 28_803_333_333)),
        (
            fields(7, 11, 47_839_123_457),
            1.1,
            fields(6, 20, 122_035_574_634),
        ),
        (
            fields(7, 11, 47_839_123_457),
            -2.7,
            fields(-2, -21, -91_318_213_073),
        ),
        (fields(-3, -5, 0), 0.333_333, fields(-9, -15, -24_624_001)),
        (fields(1200, 0, 0), 7.0, fields(171, 12, 74_057_155_200)),
        (fields(0, 10, 0), f64::INFINITY, fields(0, 0, 0)),
        (fields(0, 10, 0), f64::NEG_INFINITY, fields(0, 0, 0)),
        (
            fields(5, 0, 82_800_000_000),
            2.5,
            fields(2, 0, 33_120_000_000),
        ),
        (
            fields(0, 0, 1_000_000),
            1e-10,
            fields(0, 0, 10_000_000_000_000_000),
        ),
    ];
    for (interval, factor, expected) in cases {
        assert_eq!(
            interval.divide(factor).unwrap(),
            expected,
            "{interval:?} / {factor}"
        );
    }
}

#[test]
fn scaling_reports_overflow_nan_and_division_by_zero() {
    assert_eq!(sqlstate(fields(1, 0, 0).divide(0.0)), "22012");
    assert_eq!(sqlstate(fields(0, 0, 0).divide(-0.0)), "22012");
    assert_eq!(sqlstate(fields(1, 0, 0).divide(f64::NAN)), "22008");
    assert_eq!(sqlstate(fields(1, 0, 0).multiply(f64::NAN)), "22008");
    assert_eq!(sqlstate(fields(i32::MAX, 0, 0).multiply(2.0)), "22008");
    assert_eq!(sqlstate(fields(0, i32::MIN, 0).multiply(-1.0)), "22008");
    assert_eq!(sqlstate(fields(0, 0, i64::MAX).multiply(1.5)), "22008");
    assert_eq!(sqlstate(fields(0, 0, 1).divide(1e-300)), "22008");
}

#[test]
fn field_arithmetic_rejects_overflow_and_the_reserved_infinities() {
    assert_eq!(
        fields(1, 2, 3).plus(fields(4, 5, 6)).unwrap(),
        fields(5, 7, 9)
    );
    assert_eq!(
        fields(1, 2, 3).minus(fields(4, 5, 6)).unwrap(),
        fields(-3, -3, -3)
    );
    assert_eq!(
        sqlstate(fields(i32::MAX, 0, 0).plus(fields(1, 0, 0))),
        "22008"
    );
    assert_eq!(
        sqlstate(fields(0, 0, 0).minus(fields(0, i32::MIN, 0))),
        "22008"
    );
    assert_eq!(sqlstate(fields(0, 0, i64::MIN).negate()), "22008");
    // Finite arithmetic cannot produce the field values reserved for infinity.
    assert_eq!(
        sqlstate(fields(i32::MAX - 1, i32::MAX, i64::MAX).plus(fields(1, 0, 0))),
        "22008"
    );
    assert_eq!(
        sqlstate(fields(i32::MIN + 1, i32::MIN + 1, i64::MIN + 1).negate()),
        "22008"
    );
    assert_eq!(
        fields(i32::MIN + 1, 0, 0).negate().unwrap(),
        fields(i32::MAX, 0, 0)
    );
}

/// Each interval with its `justify_hours`, `justify_days` and `justify_interval` in `PostgreSQL` 18.
const JUSTIFIED: [(
    IntervalFields,
    IntervalFields,
    IntervalFields,
    IntervalFields,
); 14] = [
    (
        fields(0, -35, -108_000_000_000),
        fields(0, -36, -21_600_000_000),
        fields(-1, -5, -108_000_000_000),
        fields(-1, -6, -21_600_000_000),
    ),
    (
        fields(-1, 1, -90_000_000_000),
        fields(-1, 0, -3_600_000_000),
        fields(0, -29, -90_000_000_000),
        fields(-1, 0, -3_600_000_000),
    ),
    (
        fields(-1, 0, 1_000_000),
        fields(-1, 0, 1_000_000),
        fields(-1, 0, 1_000_000),
        fields(0, -29, -86_399_000_000),
    ),
    (
        fields(-1, 1, 0),
        fields(-1, 1, 0),
        fields(0, -29, 0),
        fields(0, -29, 0),
    ),
    (
        fields(2, -65, 0),
        fields(2, -65, 0),
        fields(0, -5, 0),
        fields(0, -5, 0),
    ),
    (
        fields(0, 0, -90_000_000_000),
        fields(0, -1, -3_600_000_000),
        fields(0, 0, -90_000_000_000),
        fields(0, -1, -3_600_000_000),
    ),
    (
        fields(0, -1, 3_600_000_000),
        fields(0, 0, -82_800_000_000),
        fields(0, -1, 3_600_000_000),
        fields(0, 0, -82_800_000_000),
    ),
    (
        fields(0, 0, 0),
        fields(0, 0, 0),
        fields(0, 0, 0),
        fields(0, 0, 0),
    ),
    (
        fields(1, -31, 90_000_000_000),
        fields(1, -29, -82_800_000_000),
        fields(0, -1, 90_000_000_000),
        fields(0, 0, 3_600_000_000),
    ),
    (
        fields(0, 1, -3_600_000_000),
        fields(0, 0, 82_800_000_000),
        fields(0, 1, -3_600_000_000),
        fields(0, 0, 82_800_000_000),
    ),
    (
        fields(1, -1, 0),
        fields(1, -1, 0),
        fields(0, 29, 0),
        fields(0, 29, 0),
    ),
    (
        fields(1, 0, -1_000_000),
        fields(1, 0, -1_000_000),
        fields(1, 0, -1_000_000),
        fields(0, 29, 86_399_000_000),
    ),
    (
        fields(0, 35, 108_000_000_000),
        fields(0, 36, 21_600_000_000),
        fields(1, 5, 108_000_000_000),
        fields(1, 6, 21_600_000_000),
    ),
    (
        fields(0, 59, 172_799_999_999),
        fields(0, 60, 86_399_999_999),
        fields(1, 29, 172_799_999_999),
        fields(2, 0, 86_399_999_999),
    ),
];

/// `interval_send` of `justify_hours`, `justify_days` and `justify_interval` in `PostgreSQL` 18, whose carries truncate toward zero before opposite signs trade one unit.
#[test]
fn justification_truncates_and_aligns_signs_as_postgresql_does() {
    for (interval, hours, days, both) in JUSTIFIED {
        assert_eq!(
            interval.justify_hours().unwrap(),
            hours,
            "justify_hours({interval:?})"
        );
        assert_eq!(
            interval.justify_days().unwrap(),
            days,
            "justify_days({interval:?})"
        );
        assert_eq!(
            interval.justify_interval().unwrap(),
            both,
            "justify_interval({interval:?})"
        );
    }
}

#[test]
fn justification_reports_carry_overflow() {
    assert_eq!(
        sqlstate(fields(0, i32::MAX, 86_400_000_000).justify_hours()),
        "22008"
    );
    assert_eq!(sqlstate(fields(i32::MAX, 30, 0).justify_days()), "22008");
    assert_eq!(
        sqlstate(fields(i32::MAX, 30, 1_000_000).justify_interval()),
        "22008"
    );
    assert_eq!(
        sqlstate(fields(i32::MAX, 29, 86_400_000_000).justify_interval()),
        "22008"
    );
}
