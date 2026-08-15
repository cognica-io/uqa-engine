//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn power_and_root_operators() {
    let eng = engine();
    assert_eq!(scalar(&eng, "SELECT 2 ^ 10"), Value::Float(1024.0));
    assert_eq!(text(&eng, "SELECT 2 ^ 0.5"), "1.4142135623730950");
    assert_eq!(text(&eng, "SELECT pg_typeof(2 ^ 0.5)"), "numeric");
    assert_eq!(
        text(&eng, "SELECT pg_typeof(power(2::numeric, '0.5'))"),
        "numeric"
    );
    assert_eq!(
        text(&eng, "SELECT pg_typeof(power('2', 0.5::numeric))"),
        "numeric"
    );
    assert_eq!(
        text(&eng, "SELECT pg_typeof(power('2', '0.5'))"),
        "double precision"
    );
    let error = eng.sql("SELECT power(2, NULL::text)", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(
        text(&eng, "SELECT power(0.000001::numeric, 3::numeric)"),
        "0.0000000000000000010000000000000000"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT power(2.0000000000000000000000000000000000000000::numeric, 0.5::numeric)"
        ),
        "1.4142135623730950488016887242096980785697"
    );
    assert_eq!(
        text(&eng, "SELECT power((-2)::numeric, 'NaN'::numeric)"),
        "NaN"
    );
    assert_eq!(
        text(&eng, "SELECT power((-2)::numeric, 'Infinity'::numeric)"),
        "Infinity"
    );
    assert_eq!(
        text(&eng, "SELECT power((-2)::numeric, '-Infinity'::numeric)"),
        "0"
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT length(power(1e-1000::numeric, -17::numeric)::text)"
        ),
        Value::Int(18_002)
    );
    for sql in [
        "SELECT power((-2)::numeric, 0.1::numeric)",
        "SELECT power(0::numeric, (-0.1)::numeric)",
        "SELECT power(0::numeric, (-0.5)::numeric)",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("2201F"), "{sql}: {error}");
    }
    assert_eq!(scalar(&eng, "SELECT |/ 16.0"), Value::Float(4.0));
    // glibc-compatible cbrt (PostgreSQL on Linux): last-ulp artifact.
    assert_eq!(
        scalar(&eng, "SELECT cbrt(27)"),
        Value::Float(3.000_000_000_000_000_4)
    );
}

#[test]
fn numeric_statistical_aggregates_keep_postgresql_precision() {
    let eng = engine();
    assert_eq!(
        text(
            &eng,
            "SELECT variance(x) FROM (VALUES (1), (2), (3)) AS t(x)"
        ),
        "1.00000000000000000000"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT var_pop(x) FROM (VALUES (1), (2), (3)) AS t(x)"
        ),
        "0.66666666666666666667"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT stddev_pop(x) FROM (VALUES (1), (2), (3)) AS t(x)"
        ),
        "0.81649658092772603273"
    );
    assert_eq!(
        text(&eng, "SELECT var_pop(x) FROM (VALUES (1), (1)) AS t(x)"),
        "0"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT stddev_pop(x) FROM (VALUES ('Infinity'::numeric), (1::numeric)) AS t(x)"
        ),
        "NaN"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT var_pop(x) FROM (VALUES (1e70000::numeric), (1e70000::numeric)) AS t(x)"
        ),
        "0"
    );
    assert_eq!(
        text(
            &eng,
            "SELECT var_pop(x) FROM (VALUES (1e70000::numeric), (1e70000::numeric + 1)) AS t(x)"
        ),
        "0.25000000000000000000"
    );
}
