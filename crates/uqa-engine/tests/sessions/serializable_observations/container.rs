//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Container predicates retain precise empty ranges and original keys through provider publication.

use super::*;
use uqa_core::{ArrayValue, Value};

fn array(values: Vec<Value>) -> Value {
    Value::Array(ArrayValue::try_new(values).unwrap())
}

fn tables(seed: &Session, ty: &str, initial: Option<&str>) {
    for (domain, base) in [("item_int2", "INT2VECTOR"), ("item_oid", "OIDVECTOR")] {
        if ty == domain {
            seed.sql(&format!("CREATE DOMAIN {domain} AS {base}"));
        }
    }
    for table in ["left_items", "right_items"] {
        seed.sql(&format!(
            "CREATE TABLE {table} (id INTEGER PRIMARY KEY, k {ty} UNIQUE, payload INTEGER)"
        ));
        seed.sql(&format!("CREATE INDEX {table}_k ON {table} (k)"));
        if let Some(value) = initial {
            seed.sql(&format!("INSERT INTO {table} VALUES (1, {value}, 0)"));
        }
    }
}

#[rstest::rstest]
fn empty_container_index_ranges_observe_only_matching_future_keys(
    #[values(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11)] case_index: usize,
    #[values(false, true)] conflict: bool,
) {
    let integers = || array(vec![Value::Int(1), Value::Int(2)]);
    let vector = || Value::List(vec![Value::Int(1), Value::Int(2)]);
    let cases = [
        (
            "INTEGER[]",
            Predicate::Equals(integers()),
            "ARRAY[1,2]",
            "'[0:1]={1,2}'::integer[]",
        ),
        (
            "INT2VECTOR",
            Predicate::Equals(integers()),
            "'1 2'::int2vector",
            "'1 3'::int2vector",
        ),
        (
            "OIDVECTOR",
            Predicate::Equals(integers()),
            "'1 2'::oidvector",
            "'1 3'::oidvector",
        ),
        (
            "item_int2",
            Predicate::Equals(integers()),
            "'1 2'::int2vector",
            "'1 3'::int2vector",
        ),
        (
            "item_int2",
            Predicate::Equals(vector()),
            "'1 2'::item_int2",
            "'1 3'::item_int2",
        ),
        (
            "item_oid",
            Predicate::Equals(integers()),
            "'1 2'::oidvector",
            "'1 3'::oidvector",
        ),
        (
            "item_oid",
            Predicate::Equals(vector()),
            "'1 2'::item_oid",
            "'1 3'::item_oid",
        ),
        (
            "VECTOR(2)",
            Predicate::Equals(vector()),
            "ARRAY[1,2]",
            "ARRAY[1,3]",
        ),
        (
            "TENSOR(2)",
            Predicate::Equals(Value::List(vec![vector()])),
            "ARRAY[ARRAY[1,2]]",
            "ARRAY[ARRAY[1,3]]",
        ),
        (
            "INTEGER[]",
            Predicate::Between {
                low: array(vec![Value::Int(1), Value::Float(0.5)]),
                high: array(vec![Value::Int(1), Value::Float(2.5)]),
            },
            "ARRAY[1,1]",
            "ARRAY[1,3]",
        ),
        (
            "INTEGER[]",
            Predicate::IsNull,
            "NULL",
            "ARRAY[NULL]::integer[]",
        ),
        (
            "INTEGER[]",
            Predicate::IsNotNull,
            "ARRAY[NULL]::integer[]",
            "NULL",
        ),
    ];
    let (ty, predicate, matching, outside) = &cases[case_index];
    let value = if conflict { matching } else { outside };
    let (_directory, sessions) = empty_fixtures();
    for seed in sessions {
        tables(&seed, ty, None);
        let a = seed.sibling();
        let b = seed.sibling();
        a.begin();
        b.begin();
        assert_eq!(index_only(&a, "left_items", predicate), 0);
        assert_eq!(index_only(&b, "right_items", predicate), 0);
        a.sql(&format!("INSERT INTO right_items VALUES (1, {value}, 0)"));
        b.sql(&format!("INSERT INTO left_items VALUES (1, {value}, 0)"));
        finish(&a, &b, conflict);
    }
}

#[rstest::rstest]
fn container_index_reads_retain_cold_and_cached_keys_through_delete_and_patch(
    #[values(0, 1, 2)] case_index: usize,
    #[values(false, true)] cached: bool,
    #[values(false, true)] patch: bool,
) {
    let cases = [
        (
            "INTEGER[]",
            "ARRAY[1,2]",
            "ARRAY[3,4]",
            array(vec![Value::Int(1), Value::Int(2)]),
        ),
        (
            "VECTOR(2)",
            "ARRAY[1,2]",
            "ARRAY[3,4]",
            Value::List(vec![Value::Float(1.0), Value::Float(2.0)]),
        ),
        (
            "TENSOR(2)",
            "ARRAY[ARRAY[1,2]]",
            "ARRAY[ARRAY[3,4]]",
            Value::List(vec![Value::List(vec![
                Value::Float(1.0),
                Value::Float(2.0),
            ])]),
        ),
    ];
    let (ty, original, replacement, predicate) = &cases[case_index];
    let predicate = Predicate::Equals(predicate.clone());
    let (_directory, sessions) = empty_fixtures();
    for seed in sessions {
        tables(&seed, ty, Some(original));
        let a = seed.sibling();
        let b = seed.sibling();
        if cached {
            assert_eq!(index_only(&a, "left_items", &predicate), 1);
            assert_eq!(index_only(&b, "right_items", &predicate), 1);
        }
        a.begin();
        b.begin();
        assert_eq!(index_only(&a, "left_items", &predicate), 1);
        assert_eq!(index_only(&b, "right_items", &predicate), 1);
        for (session, table) in [(&a, "right_items"), (&b, "left_items")] {
            session.sql(&if patch {
                format!("UPDATE {table} SET k = {replacement} WHERE id = 1")
            } else {
                format!("DELETE FROM {table} WHERE id = 1")
            });
        }
        assert_cycle(&a, &b);
    }
}

#[test]
fn absent_array_unique_updates_observe_element_equality_and_lower_bounds() {
    for (value, conflict) in [("ARRAY[1,2]", true), ("'[0:1]={1,2}'::integer[]", false)] {
        let (_directory, sessions) = empty_fixtures();
        for seed in sessions {
            seed.sql("CREATE DOMAIN item_array AS INTEGER[]");
            tables(&seed, "item_array", None);
            let a = seed.sibling();
            let b = seed.sibling();
            a.begin();
            b.begin();
            for (session, table) in [(&a, "left_items"), (&b, "right_items")] {
                assert_eq!(
                    session
                        .sql(&format!(
                            "UPDATE {table} SET payload = 1 WHERE k = ARRAY[1,2]"
                        ))
                        .affected_rows,
                    0
                );
            }
            a.sql(&format!("INSERT INTO right_items VALUES (1, {value}, 0)"));
            b.sql(&format!("INSERT INTO left_items VALUES (1, {value}, 0)"));
            finish(&a, &b, conflict);
        }
    }
}
