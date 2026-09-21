//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JSONB observations retain semantic index keys through private row mutations.

use super::*;
use uqa_core::{Predicate, Value};
use uqa_engine::operator_tree_bridge::EngineDriver;
use uqa_execution::operator_tree::{OperatorOutput, OperatorTreeDriver};
use uqa_operators::OperatorTree;

fn tables(seed: &Session, populated: bool) {
    for table in ["left_docs", "right_docs"] {
        seed.sql(&format!(
            "CREATE TABLE {table} (id INTEGER PRIMARY KEY, k JSONB UNIQUE, payload INTEGER)"
        ));
        seed.sql(&format!("CREATE INDEX {table}_k ON {table} (k)"));
        if populated {
            seed.sql(&format!(
                "INSERT INTO {table} VALUES (1, '{{\"a\":1,\"long\":[2]}}'::jsonb, 0)"
            ));
        }
    }
}

fn index_only(session: &Session, table: &str, predicate: &Predicate) -> usize {
    let output = EngineDriver::new(&session.engine, table, &[])
        .execute_node(&OperatorTree::IndexScan {
            index_name: format!("{table}_k"),
            field: "k".into(),
            predicate: predicate.clone(),
        })
        .unwrap();
    let OperatorOutput::Posting(posting) = output else {
        panic!("expected index postings");
    };
    posting.len()
}

fn finish(a: &Session, b: &Session, conflict: bool) {
    if conflict {
        assert_cycle(a, b);
    } else {
        a.engine.commit().unwrap();
        b.engine.commit().unwrap();
    }
}

#[test]
fn jsonb_empty_ranges_and_null_reads_observe_only_matching_writes() {
    for (predicate, matching, outside) in [
        (
            Predicate::Between {
                low: Value::JsonB("[1]".into()),
                high: Value::JsonB("[3]".into()),
            },
            "'[2]'::jsonb",
            "'[1,0]'::jsonb",
        ),
        (
            Predicate::Equals(Value::JsonB("{\"long\":[2.0],\"a\":1.00}".into())),
            "'{\"a\":1,\"long\":[2]}'::jsonb",
            "'{\"a\":3,\"long\":[2]}'::jsonb",
        ),
        (Predicate::IsNull, "NULL", "'null'::jsonb"),
        (Predicate::IsNotNull, "'null'::jsonb", "NULL"),
    ] {
        for (value, conflict) in [(matching, true), (outside, false)] {
            let (_directory, sessions) = fixtures();
            for seed in sessions {
                tables(&seed, false);
                let a = seed.sibling();
                let b = seed.sibling();
                a.begin();
                b.begin();
                assert_eq!(index_only(&a, "left_docs", &predicate), 0);
                assert_eq!(index_only(&b, "right_docs", &predicate), 0);
                a.sql(&format!("INSERT INTO right_docs VALUES (1, {value}, 0)"));
                b.sql(&format!("INSERT INTO left_docs VALUES (1, {value}, 0)"));
                finish(&a, &b, conflict);
            }
        }
    }
}

#[test]
fn jsonb_index_reads_keep_original_keys_through_delete_and_patch() {
    let predicate = Predicate::Equals(Value::JsonB("{\"long\":[2.0],\"a\":1.00}".into()));
    for cached in [false, true] {
        for mutation in [
            "DELETE FROM @ WHERE id = 1",
            "UPDATE @ SET k = '[3]'::jsonb WHERE id = 1",
        ] {
            let (_directory, sessions) = fixtures();
            for seed in sessions {
                tables(&seed, true);
                let a = seed.sibling();
                let b = seed.sibling();
                if cached {
                    assert_eq!(index_only(&a, "left_docs", &predicate), 1);
                    assert_eq!(index_only(&b, "right_docs", &predicate), 1);
                }
                a.begin();
                b.begin();
                assert_eq!(index_only(&a, "left_docs", &predicate), 1);
                assert_eq!(index_only(&b, "right_docs", &predicate), 1);
                a.sql(&mutation.replace('@', "right_docs"));
                b.sql(&mutation.replace('@', "left_docs"));
                assert_cycle(&a, &b);
            }
        }
    }
}

#[test]
fn absent_jsonb_unique_updates_keep_structural_equality() {
    for (value, conflict) in [
        ("{\"a\":1,\"long\":[2]}", true),
        ("{\"a\":3,\"long\":[2]}", false),
    ] {
        let (_directory, sessions) = fixtures();
        for seed in sessions {
            tables(&seed, false);
            let a = seed.sibling();
            let b = seed.sibling();
            a.begin();
            b.begin();
            for (session, table) in [(&a, "left_docs"), (&b, "right_docs")] {
                assert_eq!(session.sql(&format!("UPDATE {table} SET payload = 1 WHERE k = '{{\"long\":[2.0],\"a\":1.00}}'::jsonb")).affected_rows, 0);
            }
            a.sql(&format!(
                "INSERT INTO right_docs VALUES (1, '{value}'::jsonb, 0)"
            ));
            b.sql(&format!(
                "INSERT INTO left_docs VALUES (1, '{value}'::jsonb, 0)"
            ));
            finish(&a, &b, conflict);
        }
    }
}
