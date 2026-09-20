//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected index predicates conflict with old/new keys, including empty and retained-cache results.

use super::*;
use uqa_core::{Predicate, Value};
use uqa_engine::operator_tree_bridge::EngineDriver;
use uqa_execution::operator_tree::{OperatorOutput, OperatorTreeDriver};
use uqa_operators::OperatorTree;

fn indexes(session: &Session) {
    session.sql("CREATE INDEX left_t_v ON left_t (v)");
    session.sql("CREATE INDEX right_t_v ON right_t (v)");
}

fn index_only(session: &Session, table: &str, value: i64) -> usize {
    let output = EngineDriver::new(&session.engine, table, &[])
        .execute_node(&OperatorTree::IndexScan {
            index_name: format!("{table}_v"),
            field: "v".into(),
            predicate: Predicate::Equals(Value::Int(value)),
        })
        .unwrap();
    let OperatorOutput::Posting(posting) = output else {
        panic!("expected index postings");
    };
    posting.len()
}

fn warm(a: &Session, b: &Session) {
    for session in [a, b] {
        for table in ["left_t", "right_t"] {
            assert_eq!(index_only(session, table, -123), 0);
        }
    }
}

#[test]
fn indexed_sql_empty_ranges_conflict_only_with_matching_future_keys() {
    for cached in [false, true] {
        for value in [99, 200] {
            let (_directory, sessions) = fixtures();
            for seed in sessions {
                indexes(&seed);
                let a = seed.sibling();
                let b = seed.sibling();
                a.begin();
                b.begin();
                if cached {
                    warm(&a, &b);
                }
                assert!(a
                    .sql("SELECT v FROM left_t WHERE v BETWEEN 90 AND 110")
                    .rows
                    .is_empty());
                assert!(b
                    .sql("SELECT v FROM right_t WHERE v BETWEEN 90 AND 110")
                    .rows
                    .is_empty());
                a.sql(&format!("INSERT INTO right_t VALUES (2, {value})"));
                b.sql(&format!("INSERT INTO left_t VALUES (2, {value})"));
                if value == 99 {
                    assert_cycle(&a, &b);
                } else {
                    a.engine.commit().unwrap();
                    b.engine.commit().unwrap();
                }
            }
        }
    }
}

#[test]
fn index_only_reads_track_original_keys_through_delete_rewrite_and_row_movement() {
    for cached in [false, true] {
        for mutation in [
            "DELETE FROM @ WHERE id = 1",
            "UPDATE @ SET v = 2 WHERE id = 1",
            "UPDATE @ SET v = v + 1 WHERE id = 1 RETURNING v",
            "UPDATE @ SET id = 2 WHERE id = 1",
        ] {
            let (_directory, sessions) = fixtures();
            for seed in sessions {
                indexes(&seed);
                let a = seed.sibling();
                let b = seed.sibling();
                a.begin();
                b.begin();
                if cached {
                    warm(&a, &b);
                }
                assert_eq!(index_only(&a, "left_t", 1), 1);
                assert_eq!(index_only(&b, "right_t", 1), 1);
                a.sql(&mutation.replace('@', "right_t"));
                b.sql(&mutation.replace('@', "left_t"));
                assert_cycle(&a, &b);
            }
        }
    }
}

#[test]
fn rolled_back_index_intents_do_not_conflict_with_later_range_reads() {
    let (_directory, sessions) = fixtures();
    for seed in sessions {
        indexes(&seed);
        let a = seed.sibling();
        let b = seed.sibling();
        a.begin();
        b.begin();
        assert_eq!(index_only(&a, "left_t", 99), 0);
        a.sql("SAVEPOINT changed_keys");
        a.sql("INSERT INTO right_t VALUES (2, 99)");
        a.sql("ROLLBACK TO SAVEPOINT changed_keys");
        a.sql("RELEASE SAVEPOINT changed_keys");
        assert_eq!(index_only(&b, "right_t", 99), 0);
        b.sql("INSERT INTO left_t VALUES (2, 99)");
        a.engine.commit().unwrap();
        b.engine.commit().unwrap();
    }
}

#[test]
fn absent_point_updates_observe_only_the_selected_unique_key() {
    for field in ["id", "v"] {
        for value in [99, 200] {
            let (_directory, sessions) = fixtures();
            for seed in sessions {
                for table in ["left_t", "right_t"] {
                    seed.sql(&format!("ALTER TABLE {table} ADD COLUMN payload INTEGER"));
                    seed.sql(&format!("CREATE UNIQUE INDEX {table}_v ON {table} (v)"));
                }
                let a = seed.sibling();
                let b = seed.sibling();
                a.begin();
                b.begin();
                for (session, table) in [(&a, "left_t"), (&b, "right_t")] {
                    assert_eq!(
                        session
                            .sql(&format!(
                                "UPDATE {table} SET payload = 7 WHERE {field} = 99"
                            ))
                            .affected_rows,
                        0
                    );
                }
                a.sql(&format!("INSERT INTO right_t VALUES ({value}, {value}, 0)"));
                b.sql(&format!("INSERT INTO left_t VALUES ({value}, {value}, 0)"));
                if value == 99 {
                    assert_cycle(&a, &b);
                } else {
                    a.engine.commit().unwrap();
                    b.engine.commit().unwrap();
                }
            }
        }
    }
}

#[test]
fn missing_durable_column_postings_keep_repaired_read_dependencies() {
    let (_directory, sessions) = fixtures();
    for seed in sessions {
        indexes(&seed);
        let field = uqa_storage::ValueIndexKey::Column("v".into());
        for table in ["public.left_t", "public.right_t"] {
            assert_eq!(
                seed.backend
                    .read_btree_index_entry(table, &field, 1)
                    .unwrap(),
                uqa_storage::ValueIndexEntry::Present(Value::Int(1))
            );
            seed.backend
                .replace_btree_index(table, &field, &[])
                .unwrap();
            assert_eq!(
                seed.backend
                    .read_btree_index_entry(table, &field, 1)
                    .unwrap(),
                uqa_storage::ValueIndexEntry::Absent
            );
        }
        let a = seed.sibling();
        let b = seed.sibling();
        a.begin();
        b.begin();
        assert_eq!(index_only(&a, "left_t", 1), 1);
        assert_eq!(index_only(&b, "right_t", 1), 1);
        for table in ["public.left_t", "public.right_t"] {
            assert_eq!(
                seed.backend
                    .read_btree_index_entry(table, &field, 1)
                    .unwrap(),
                uqa_storage::ValueIndexEntry::Absent
            );
        }
        a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
        b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
        assert_cycle(&a, &b);
    }
}
