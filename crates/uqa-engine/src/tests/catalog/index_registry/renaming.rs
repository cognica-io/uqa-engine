//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index names change independently of physical keys and transaction-owned index locks.

use super::{definition, error, sessions, sql, Arc, Engine, Value};
use crate::tests::relation_lock_support::after_index_wait;

#[test]
fn explicit_index_rename_preserves_owned_and_independent_foreign_key_targets() {
    for provider in 0..3 {
        for owned in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                if owned {
                    "ALTER TABLE t ADD CONSTRAINT idx UNIQUE(v)"
                } else {
                    "CREATE UNIQUE INDEX idx ON t(v)"
                },
            );
            sql(&first, "CREATE TABLE child(v int REFERENCES t(v))");
            let original = definition(&first, "idx");
            let oid = original.catalog.as_ref().unwrap().identity.oid;
            sql(&first, "BEGIN; SAVEPOINT saved");
            let renamed = sql(&first, "ALTER INDEX idx RENAME TO \"Case.Index\"");
            assert_eq!(renamed.command_tag.as_deref(), Some("ALTER INDEX"));
            assert_eq!(definition(&first, "\"Case.Index\""), original);
            assert_eq!(
                sql(
                    &first,
                    "SELECT conindid FROM pg_constraint WHERE contype='f'"
                )
                .rows[0]["conindid"],
                Value::Int(oid)
            );
            if owned {
                assert_eq!(
                    first.key_constraints("t").unwrap()[0].name.as_deref(),
                    Some("Case.Index")
                );
            }
            sql(&first, "ROLLBACK TO saved");
            assert_eq!(definition(&first, "idx"), original);
            sql(&first, "ALTER INDEX idx RENAME TO \"Case.Index\"; COMMIT");
            assert_eq!(definition(&second, "\"Case.Index\""), original);
            if owned {
                sql(
                    &second,
                    "INSERT INTO t VALUES(1) ON CONFLICT ON CONSTRAINT \"Case.Index\" DO NOTHING",
                );
            }
            error(&second, "INSERT INTO t VALUES(1)", "23505");
            error(&second, "INSERT INTO child VALUES(99)", "23503");
            error(&second, "DROP INDEX \"Case.Index\"", "2BP01");
            let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
            drop((first, second));
            let reopened = Engine::from_persistent_provider(factory).unwrap();
            assert_eq!(definition(&reopened, "\"Case.Index\""), original);
            if owned {
                sql(
                    &reopened,
                    "INSERT INTO t VALUES(1) ON CONFLICT ON CONSTRAINT \"Case.Index\" DO NOTHING",
                );
            }
            sql(
                &reopened,
                if owned {
                    "ALTER TABLE t DROP CONSTRAINT \"Case.Index\" CASCADE"
                } else {
                    "DROP INDEX \"Case.Index\" CASCADE"
                },
            );
            sql(
                &reopened,
                "INSERT INTO child VALUES(99); INSERT INTO t VALUES(1)",
            );
        }
    }
}

#[test]
fn index_rename_keeps_partition_names_local_and_preserves_search_fields() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(k int CONSTRAINT pk PRIMARY KEY, body text, embedding vector(2)) PARTITION BY RANGE(k); CREATE TABLE c PARTITION OF p FOR VALUES FROM(0) TO(10); INSERT INTO p VALUES(1,'alpha',ARRAY[1.0,0.0]); CREATE INDEX text_idx ON p USING gin(body); CREATE INDEX vector_idx ON p USING hnsw(embedding)");
        let key = definition(&first, "pk");
        let text = definition(&first, "text_idx");
        let vector = definition(&first, "vector_idx");
        let child_name = first.key_constraints("c").unwrap()[0].name.clone().unwrap();
        let child = definition(&first, &child_name);
        sql(&first, "ALTER INDEX pk RENAME TO moved_pk; ALTER TABLE text_idx RENAME TO moved_text; ALTER INDEX vector_idx RENAME TO moved_vector");
        assert_eq!(definition(&first, "moved_pk"), key);
        assert_eq!(definition(&first, "moved_text"), text);
        assert_eq!(definition(&first, "moved_vector"), vector);
        assert_eq!(definition(&first, &child_name), child);
        sql(
            &first,
            &format!("ALTER INDEX {child_name} RENAME TO local_child"),
        );
        assert_eq!(definition(&first, "local_child"), child);
        assert_eq!(definition(&first, "moved_pk"), key);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(
            sql(
                &reopened,
                "SELECT count(*) AS n FROM p WHERE text_match(body,'alpha')"
            )
            .rows[0]["n"],
            Value::Int(1)
        );
        assert_eq!(definition(&reopened, "local_child"), child);
        error(&reopened, "INSERT INTO p(k) VALUES(1)", "23505");
        sql(&reopened, "DROP INDEX moved_text, moved_vector");
        assert!(reopened.fts_fields_for_table("c").unwrap().is_empty());
    }
}

#[test]
fn index_rename_checks_namespace_authority_conflicts_and_read_only_state() {
    for provider in 0..3 {
        let (_directory, first, _) = sessions(provider);
        sql(&first, "CREATE SCHEMA app; CREATE ROLE restricted; CREATE TABLE app.items(v int CONSTRAINT idx UNIQUE, CONSTRAINT occupied CHECK(v>0)); CREATE TABLE app.taken(v int)");
        error(&first, "ALTER INDEX app.idx RENAME TO occupied", "42710");
        error(&first, "ALTER INDEX app.idx RENAME TO taken", "42P07");
        error(&first, "ALTER INDEX app.idx RENAME TO idx", "42P07");
        sql(&first, "ALTER TABLE app.items OWNER TO restricted; GRANT USAGE ON SCHEMA app TO restricted; SET ROLE restricted");
        error(&first, "ALTER INDEX app.idx RENAME TO renamed", "42501");
        sql(&first, "RESET ROLE; GRANT CREATE ON SCHEMA app TO restricted; SET ROLE restricted; ALTER INDEX app.idx RENAME TO renamed; RESET ROLE");
        sql(&first, "ALTER INDEX IF EXISTS app.absent RENAME TO ignored");
        error(&first, "ALTER INDEX app.absent RENAME TO ignored", "42P01");
        sql(&first, "BEGIN READ ONLY");
        error(
            &first,
            "ALTER INDEX app.renamed RENAME TO forbidden",
            "25006",
        );
        sql(&first, "ROLLBACK");
        assert!(first.catalog_index("app.renamed").unwrap().is_some());
    }
}

#[test]
fn index_rename_allows_a_peer_document_commit_without_losing_enforcement() {
    for provider in 0..3 {
        for owned in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                if owned {
                    "ALTER TABLE t ADD CONSTRAINT idx UNIQUE(v)"
                } else {
                    "CREATE UNIQUE INDEX idx ON t(v)"
                },
            );
            let original = definition(&first, "idx");
            sql(&first, "BEGIN; ALTER INDEX idx RENAME TO renamed");
            sql(&second, "INSERT INTO t VALUES(2)");
            first
                .sql("COMMIT", &[])
                .unwrap_or_else(|error| panic!("provider {provider}, owned {owned}: {error}"));
            assert_eq!(definition(&second, "renamed"), original);
            assert_eq!(
                sql(&first, "SELECT count(*) AS n FROM t").rows[0]["n"],
                Value::Int(2)
            );
            error(&first, "INSERT INTO t VALUES(2)", "23505");
        }
    }
}

#[test]
fn explicit_names_rebind_after_an_index_rename_wait_and_savepoint_undo() {
    for provider in 0..3 {
        for commit in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE UNIQUE INDEX idx ON t(v)");
            let original = definition(&first, "idx");
            sql(&first, "BEGIN; ALTER INDEX idx RENAME TO moved");
            let (second, result) = after_index_wait(
                &first,
                second,
                "BEGIN; SAVEPOINT retained; ALTER INDEX idx RENAME TO final_name",
                original.catalog.as_ref().unwrap().identity.object_id,
                if commit { "COMMIT" } else { "ROLLBACK" },
            );
            if commit {
                assert_eq!(result.unwrap_err().sqlstate(), Some("42P01"));
            } else {
                result.unwrap();
                assert_eq!(definition(&second, "final_name"), original);
            }
            sql(&second, "ROLLBACK TO retained; COMMIT");
            assert_eq!(
                definition(&first, if commit { "moved" } else { "idx" }),
                original
            );
        }
    }
}

#[test]
fn index_statement_rebinds_a_replacement_table_with_table_locking() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE INDEX idx ON t(v)");
        let original = definition(&first, "idx");
        sql(&first, "BEGIN; ALTER INDEX idx RENAME TO moved; CREATE TABLE idx(v int); INSERT INTO idx VALUES(9)");
        let (second, result) = after_index_wait(
            &first,
            second,
            "ALTER INDEX idx RENAME TO renamed_table",
            original.catalog.as_ref().unwrap().identity.object_id,
            "COMMIT",
        );
        result.unwrap();
        assert_eq!(definition(&second, "moved"), original);
        assert_eq!(
            sql(&second, "SELECT v FROM renamed_table").rows[0]["v"],
            Value::Int(9)
        );
    }
}

#[test]
fn index_deletion_waits_for_rename_then_releases_the_superseded_heap_lock() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE INDEX idx ON t(v)");
        let original = definition(&first, "idx");
        sql(&first, "BEGIN; ALTER INDEX idx RENAME TO moved");
        let (second, result) = after_index_wait(
            &first,
            second,
            "BEGIN; DROP INDEX idx",
            original.catalog.as_ref().unwrap().identity.object_id,
            "COMMIT",
        );
        assert_eq!(result.unwrap_err().sqlstate(), Some("42704"));
        sql(
            &first,
            "BEGIN; LOCK TABLE t IN ACCESS SHARE MODE NOWAIT; ROLLBACK",
        );
        sql(&second, "ROLLBACK");
        assert_eq!(definition(&first, "moved"), original);
    }
}

#[test]
fn distinct_indexes_on_one_table_can_rename_and_commit_independently() {
    for provider in 0..3 {
        for owned in [false, true] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE TABLE paired(v int,w int)");
            sql(
                &first,
                if owned {
                    "ALTER TABLE paired ADD CONSTRAINT one UNIQUE(v), ADD CONSTRAINT two UNIQUE(w)"
                } else {
                    "CREATE UNIQUE INDEX one ON paired(v); CREATE UNIQUE INDEX two ON paired(w)"
                },
            );
            let one = definition(&first, "one");
            let two = definition(&first, "two");
            sql(&first, "BEGIN; ALTER INDEX one RENAME TO first_name");
            sql(&second, "ALTER INDEX two RENAME TO second_name");
            first
                .sql("COMMIT", &[])
                .unwrap_or_else(|error| panic!("provider {provider}, owned {owned}: {error}"));
            assert_eq!(definition(&first, "first_name"), one);
            assert_eq!(definition(&first, "second_name"), two);
            sql(&second, "INSERT INTO paired VALUES(1,2)");
            error(&second, "INSERT INTO paired VALUES(1,3)", "23505");
            error(&second, "INSERT INTO paired VALUES(4,2)", "23505");
        }
    }
}

#[test]
fn key_constraint_rename_retains_its_index_after_waiting_for_an_index_rename() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT idx UNIQUE(v)");
        let original = definition(&first, "idx");
        sql(&first, "BEGIN; ALTER INDEX idx RENAME TO moved");
        let (second, result) = after_index_wait(
            &first,
            second,
            "ALTER TABLE t RENAME CONSTRAINT idx TO final_name",
            original.catalog.as_ref().unwrap().identity.object_id,
            "COMMIT",
        );
        result.unwrap();
        assert_eq!(definition(&second, "final_name"), original);
    }
}

#[test]
fn table_deletion_waits_for_an_index_rename_before_removing_the_current_graph() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE INDEX idx ON t(v)");
        let original = definition(&first, "idx");
        sql(&first, "BEGIN; ALTER INDEX idx RENAME TO moved");
        let (second, result) = after_index_wait(
            &first,
            second,
            "DROP TABLE t",
            original.catalog.as_ref().unwrap().identity.object_id,
            "COMMIT",
        );
        result.unwrap();
        assert!(second.catalog_index("moved").unwrap().is_none());
        assert!(!second.has_table("t").unwrap());
    }
}
