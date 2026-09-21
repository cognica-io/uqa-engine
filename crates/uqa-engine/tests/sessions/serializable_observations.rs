//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL access paths retain original participants through provider session adapters.

use std::sync::Arc;
use uqa_core::Predicate;
use uqa_engine::{operator_tree_bridge::EngineDriver, Engine};
use uqa_execution::operator_tree::{OperatorOutput, OperatorTreeDriver};
use uqa_operators::OperatorTree;
use uqa_storage::{PersistentStorageBackend, PersistentStorageProvider, PersistentStorageSession};
use uqa_storage_redb::RedbStorage;
use uqa_storage_sqlite::{ManagedConnection, SQLiteKeyValueStorage, SQLiteStorageProvider};

#[path = "serializable_observations/container.rs"]
mod container;

#[path = "serializable_observations/indexed.rs"]
mod indexed;

#[path = "serializable_observations/jsonb.rs"]
mod jsonb;

#[path = "serializable_observations/temporal.rs"]
mod temporal;

#[path = "serializable_observations/vector.rs"]
mod vector;

#[path = "serializable_observations/text.rs"]
mod text;

#[path = "serializable_observations/graph.rs"]
mod graph;

#[path = "serializable_observations/admission.rs"]
mod admission;

#[path = "serializable_observations/direct_reads.rs"]
mod direct_reads;

#[path = "serializable_observations/direct_writes.rs"]
mod direct_writes;

#[path = "serializable_observations/direct_queries.rs"]
mod direct_queries;

#[path = "serializable_observations/catalog_reads.rs"]
mod catalog_reads;

struct Session {
    engine: Engine,
    backend: Arc<dyn PersistentStorageBackend>,
    catalog: Arc<dyn uqa_storage::CatalogFacade>,
}

impl Session {
    fn new(pair: PersistentStorageSession) -> Self {
        let backend = pair.backend.clone();
        let catalog = pair.catalog.clone();
        let engine = Engine::from_persistent_backends(pair.catalog, pair.backend).unwrap();
        Self {
            engine,
            backend,
            catalog,
        }
    }

    fn sibling(&self) -> Self {
        Self::new(self.backend.open_session().unwrap())
    }

    fn begin(&self) {
        self.engine
            .sql("BEGIN ISOLATION LEVEL SERIALIZABLE", &[])
            .unwrap();
        // These access-path schedules deliberately select their first snapshot before either session starts its tested access.
        self.sql("SELECT 1");
    }

    fn sql(&self, sql: &str) -> uqa_sql::SQLResult {
        self.engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
    }
}

fn fixtures() -> (tempfile::TempDir, Vec<Session>) {
    let directory = tempfile::tempdir().unwrap();
    let providers: Vec<Box<dyn PersistentStorageProvider>> = vec![
        Box::new(SQLiteStorageProvider::new(
            ManagedConnection::open_in_memory().unwrap(),
        )),
        Box::new(SQLiteKeyValueStorage::open_in_memory().unwrap()),
        Box::new(RedbStorage::open(directory.path().join("observations.redb")).unwrap()),
    ];
    let sessions = providers
        .into_iter()
        .map(|provider| {
            let session = Session::new(provider.open_session().unwrap());
            session.sql("CREATE TABLE left_t (id INTEGER PRIMARY KEY, v INTEGER)");
            session.sql("CREATE TABLE right_t (id INTEGER PRIMARY KEY, v INTEGER)");
            session.sql("INSERT INTO left_t VALUES (1, 1)");
            session.sql("INSERT INTO right_t VALUES (1, 1)");
            session
        })
        .collect();
    (directory, sessions)
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

fn assert_cycle(a: &Session, b: &Session) {
    a.engine.commit().unwrap();
    let error = b.engine.commit().unwrap_err();
    assert_eq!(error.sqlstate(), Some("40001"), "{error}");
    assert_eq!(a.engine.transaction_depth(), 0);
    assert_eq!(b.engine.transaction_depth(), 0);
}

#[test]
fn admitted_sql_scans_conflict_with_insert_delete_patch_rewrite_and_row_movement() {
    for mutation in [
        "INSERT INTO @ VALUES (2, 2)",
        "DELETE FROM @ WHERE id = 1",
        "UPDATE @ SET v = 2 WHERE id = 1",
        "UPDATE @ SET v = v + 1 WHERE id = 1 RETURNING v",
        "UPDATE @ SET id = 2 WHERE id = 1",
    ] {
        let (_directory, fixtures) = fixtures();
        for a in fixtures {
            let b = a.sibling();
            a.begin();
            b.begin();
            assert_eq!(a.sql("SELECT v FROM left_t").rows.len(), 1);
            assert_eq!(b.sql("SELECT v FROM right_t").rows.len(), 1);
            a.sql(&mutation.replace('@', "right_t"));
            b.sql(&mutation.replace('@', "left_t"));
            assert_cycle(&a, &b);
        }
    }
}

#[test]
fn admitted_empty_scans_retain_phantom_observations() {
    let (_directory, fixtures) = fixtures();
    for a in fixtures {
        let b = a.sibling();
        a.begin();
        b.begin();
        assert!(a.sql("SELECT id FROM left_t WHERE v = 99").rows.is_empty());
        assert!(b.sql("SELECT id FROM right_t WHERE v = 99").rows.is_empty());
        a.sql("INSERT INTO right_t VALUES (2, 99)");
        b.sql("INSERT INTO left_t VALUES (2, 99)");
        assert_cycle(&a, &b);
    }
}

#[test]
fn admitted_aggregate_and_join_sources_keep_their_logical_reads() {
    for query in [
        "SELECT COUNT(*) AS n FROM @",
        "SELECT SUM(v) AS n FROM @",
        "SELECT x.v FROM @ x CROSS JOIN (VALUES (1)) AS y(n)",
    ] {
        let (_directory, fixtures) = fixtures();
        for a in fixtures {
            let b = a.sibling();
            a.begin();
            b.begin();
            assert_eq!(a.sql(&query.replace('@', "left_t")).rows.len(), 1);
            assert_eq!(b.sql(&query.replace('@', "right_t")).rows.len(), 1);
            a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
            b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
            assert_cycle(&a, &b);
        }
    }
}

#[test]
fn admitted_unused_scans_and_rolled_back_row_intents_do_not_create_cycles() {
    for rollback_write in [false, true] {
        let (_directory, fixtures) = fixtures();
        for a in fixtures {
            let b = a.sibling();
            a.begin();
            b.begin();
            if rollback_write {
                a.sql("SELECT v FROM left_t");
            } else {
                assert!(a.sql("SELECT v FROM left_t LIMIT 0").rows.is_empty());
            }
            if !rollback_write {
                b.sql("SELECT v FROM right_t");
            }
            a.sql("SAVEPOINT write_intent");
            a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
            if rollback_write {
                a.sql("ROLLBACK TO SAVEPOINT write_intent");
                // A later reader must not encounter an intent that savepoint undo removed.
                b.sql("SELECT v FROM right_t");
            }
            a.sql("RELEASE SAVEPOINT write_intent");
            b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
            a.engine.commit().unwrap();
            b.engine.commit().unwrap();
            assert_eq!(
                a.sql("SELECT v FROM left_t").rows[0]["v"],
                uqa_core::Value::Int(2)
            );
            assert_eq!(
                a.sql("SELECT v FROM right_t").rows[0]["v"],
                uqa_core::Value::Int(if rollback_write { 1 } else { 2 })
            );
        }
    }
}

#[test]
fn admitted_savepoint_undo_preserves_previously_observed_dependencies() {
    let (_directory, fixtures) = fixtures();
    for a in fixtures {
        let b = a.sibling();
        a.begin();
        b.begin();
        a.sql("SELECT v FROM left_t");
        b.sql("SELECT v FROM right_t");
        a.sql("SAVEPOINT write_intent");
        a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
        a.sql("ROLLBACK TO SAVEPOINT write_intent");
        a.sql("RELEASE SAVEPOINT write_intent");
        b.sql("UPDATE left_t SET v = 2 WHERE id = 1");
        assert_cycle(&a, &b);
        for table in ["left_t", "right_t"] {
            assert_eq!(
                a.sql(&format!("SELECT v FROM {table}")).rows[0]["v"],
                uqa_core::Value::Int(1)
            );
        }
    }
}

#[test]
fn admitted_independent_relations_preserve_both_commits() {
    let (_directory, fixtures) = fixtures();
    for a in fixtures {
        let b = a.sibling();
        a.begin();
        b.begin();
        a.sql("SELECT v FROM left_t");
        b.sql("SELECT v FROM right_t");
        a.sql("UPDATE left_t SET v = 2 WHERE id = 1");
        b.sql("UPDATE right_t SET v = 2 WHERE id = 1");
        a.engine.commit().unwrap();
        b.engine.commit().unwrap();
        assert_eq!(
            a.sql("SELECT v FROM left_t").rows[0]["v"],
            uqa_core::Value::Int(2)
        );
        assert_eq!(
            a.sql("SELECT v FROM right_t").rows[0]["v"],
            uqa_core::Value::Int(2)
        );
    }
}
