use super::*;

#[test]
fn numeric_create_table() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, price NUMERIC(10, 2))",
        &[],
    )
    .unwrap();
    let r = eng.sql("SELECT * FROM t", &[]).unwrap();
    assert!(r.columns.iter().any(|c| c == "price"));
}

#[test]
fn numeric_insert_rounds_to_scale() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, price NUMERIC(10, 2))",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO t (id, price) VALUES (1, 19.999)", &[])
        .unwrap();
    let r = eng.sql("SELECT price FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(decimal_col(&r.rows[0], "price"), Some(dec("20.00")));
}

#[test]
fn numeric_insert_preserves_scale() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, amount NUMERIC(8, 3))",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO t (id, amount) VALUES (1, 123.456)", &[])
        .unwrap();
    let r = eng.sql("SELECT amount FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(decimal_col(&r.rows[0], "amount"), Some(dec("123.456")));
}

#[test]
fn numeric_arithmetic() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a NUMERIC(10, 2), b NUMERIC(10, 2))",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO t (id, a, b) VALUES (1, 10.50, 3.25)", &[])
        .unwrap();
    let r = eng
        .sql("SELECT a + b AS total FROM t WHERE id = 1", &[])
        .unwrap();
    assert_eq!(decimal_col(&r.rows[0], "total"), Some(dec("13.75")));
}

#[test]
fn numeric_round_accepts_negative_scale() {
    let eng = engine();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v NUMERIC)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO t (id, v) VALUES (1, 1234.56), (2, 1250.00), (3, -1250.00)",
        &[],
    )
    .unwrap();

    let r = eng
        .sql("SELECT round(v, -2) AS rounded FROM t ORDER BY id", &[])
        .unwrap();
    assert_eq!(decimal_col(&r.rows[0], "rounded"), Some(dec("1200")));
    assert_eq!(decimal_col(&r.rows[1], "rounded"), Some(dec("1300")));
    assert_eq!(decimal_col(&r.rows[2], "rounded"), Some(dec("-1300")));
}

#[test]
fn numeric_trunc_preserves_decimal_and_accepts_negative_scale() {
    let eng = engine();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v NUMERIC)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO t (id, v) VALUES (1, 1234.567), (2, -1234.567)",
        &[],
    )
    .unwrap();

    let r = eng
        .sql(
            "SELECT trunc(v) AS whole, trunc(v, 2) AS cents, trunc(v, -2) AS hundreds
             FROM t ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(decimal_col(&r.rows[0], "whole"), Some(dec("1234")));
    assert_eq!(decimal_col(&r.rows[0], "cents"), Some(dec("1234.56")));
    assert_eq!(decimal_col(&r.rows[0], "hundreds"), Some(dec("1200")));
    assert_eq!(decimal_col(&r.rows[1], "whole"), Some(dec("-1234")));
    assert_eq!(decimal_col(&r.rows[1], "cents"), Some(dec("-1234.56")));
    assert_eq!(decimal_col(&r.rows[1], "hundreds"), Some(dec("-1200")));
}

#[test]
fn numeric_comparison() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val NUMERIC(10, 2))",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO t (id, val) VALUES (1, 10.50), (2, 20.75), (3, 5.25)",
        &[],
    )
    .unwrap();
    let r = eng
        .sql("SELECT id FROM t WHERE val > 10.00 ORDER BY id", &[])
        .unwrap();
    let ids: Vec<i64> = r.rows.iter().filter_map(|row| int_col(row, "id")).collect();
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn numeric_no_scale_specified() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val NUMERIC(10))",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO t (id, val) VALUES (1, 42.9)", &[])
        .unwrap();
    let r = eng.sql("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(decimal_col(&r.rows[0], "val"), Some(dec("43")));
}

#[test]
fn plain_numeric_no_precision() {
    let eng = engine();
    eng.sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val NUMERIC)", &[])
        .unwrap();
    eng.sql("INSERT INTO t (id, val) VALUES (1, 3.125)", &[])
        .unwrap();
    let r = eng.sql("SELECT val FROM t WHERE id = 1", &[]).unwrap();
    assert_eq!(decimal_col(&r.rows[0], "val"), Some(dec("3.125")));
}

#[test]
fn numeric_sum_avg_min_max_return_decimal() {
    let eng = engine();
    eng.sql("CREATE TABLE t (amount NUMERIC(10, 2))", &[])
        .unwrap();
    eng.sql("INSERT INTO t (amount) VALUES (0.10), (0.20), (0.30)", &[])
        .unwrap();
    let r = eng
        .sql(
            "SELECT SUM(amount) AS total, AVG(amount) AS average,
                    MIN(amount) AS smallest, MAX(amount) AS largest
             FROM t",
            &[],
        )
        .unwrap();
    assert_eq!(decimal_col(&r.rows[0], "total"), Some(dec("0.60")));
    assert_eq!(decimal_col(&r.rows[0], "average"), Some(dec("0.20")));
    assert_eq!(decimal_col(&r.rows[0], "smallest"), Some(dec("0.10")));
    assert_eq!(decimal_col(&r.rows[0], "largest"), Some(dec("0.30")));
}

#[test]
fn stddev_samp() {
    let eng = engine_with_table();
    let r = eng.sql("SELECT stddev(val) AS v FROM t", &[]).unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    assert!((v - 10.0).abs() < 0.001);
}

#[test]
fn stddev_pop() {
    let eng = engine_with_table();
    let r = eng.sql("SELECT stddev_pop(val) AS v FROM t", &[]).unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    let expected = (200.0_f64 / 3.0).sqrt();
    assert!((v - expected).abs() < 0.001);
}

#[test]
fn stddev_single_row_is_null() {
    let eng = engine_with_table();
    let r = eng
        .sql("SELECT stddev(val) AS v FROM t WHERE id = 1", &[])
        .unwrap();
    assert!(matches!(r.rows[0].get("v"), Some(Value::Null) | None));
}

#[test]
fn variance_samp() {
    let eng = engine_with_table();
    let r = eng.sql("SELECT variance(val) AS v FROM t", &[]).unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    assert!((v - 100.0).abs() < 0.001);
}

#[test]
fn variance_pop() {
    let eng = engine_with_table();
    let r = eng.sql("SELECT var_pop(val) AS v FROM t", &[]).unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    let expected = 200.0 / 3.0;
    assert!((v - expected).abs() < 0.001);
}

#[test]
fn percentile_cont_median() {
    let eng = engine_with_table();
    let r = eng
        .sql(
            "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY val) AS v FROM t",
            &[],
        )
        .unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    assert!((v - 20.0).abs() < 0.001);
}

#[test]
fn percentile_cont_quartile() {
    let eng = engine_with_table();
    let r = eng
        .sql(
            "SELECT percentile_cont(0.25) WITHIN GROUP (ORDER BY val) AS v FROM t",
            &[],
        )
        .unwrap();
    let v = float_col(&r.rows[0], "v").unwrap();
    assert!((v - 15.0).abs() < 0.001);
}

#[test]
fn percentile_disc_median() {
    let eng = engine_with_table();
    let r = eng
        .sql(
            "SELECT percentile_disc(0.5) WITHIN GROUP (ORDER BY val) AS v FROM t",
            &[],
        )
        .unwrap();
    assert_eq!(int_col(&r.rows[0], "v"), Some(20));
}

#[test]
fn mode_basic() {
    let eng = engine();
    eng.sql(
        "CREATE TABLE m (id BIGSERIAL PRIMARY KEY, val INTEGER)",
        &[],
    )
    .unwrap();
    eng.sql("INSERT INTO m (val) VALUES (1), (2), (2), (3)", &[])
        .unwrap();
    let r = eng
        .sql("SELECT mode() WITHIN GROUP (ORDER BY val) AS v FROM m", &[])
        .unwrap();
    assert_eq!(int_col(&r.rows[0], "v"), Some(2));
}
