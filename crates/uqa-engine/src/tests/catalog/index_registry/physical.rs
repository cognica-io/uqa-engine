//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical partition indexes and schema changes across persistent catalog refresh.

use super::{definition, sessions, sql, Arc, Engine, Value};
use uqa_storage::vector_index::{HNSWIndexParams, VectorIndexOpenMode, VectorIndexSpec};

#[test]
fn partition_search_indexes_build_existing_and_new_children_and_drop_shared_fields() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(k int, body text, embedding vector(2)) PARTITION BY RANGE(k); CREATE TABLE c PARTITION OF p FOR VALUES FROM(0) TO(10); INSERT INTO p VALUES(1,'alpha',ARRAY[1.0,0.0]); CREATE INDEX root_text ON p USING gin(body); CREATE INDEX root_vector ON p USING hnsw(embedding); CREATE TABLE d PARTITION OF p FOR VALUES FROM(10) TO(20); INSERT INTO p VALUES(11,'beta',ARRAY[0.0,1.0])");
        for (table, word) in [("c", "alpha"), ("d", "beta")] {
            assert_eq!(
                sql(
                    &first,
                    &format!("SELECT count(*) AS n FROM {table} WHERE text_match(body,'{word}')")
                )
                .rows[0]["n"],
                Value::Int(1)
            );
            assert!(first
                .storage
                .backend
                .as_ref()
                .unwrap()
                .vector_index(
                    &format!("public.{table}"),
                    "embedding",
                    2,
                    VectorIndexSpec::HNSW(HNSWIndexParams::default()),
                    VectorIndexOpenMode::Restore
                )
                .is_ok());
        }
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        assert_eq!(sql(&reopened, "SELECT count(*) AS n FROM p WHERE text_match(body,'alpha') OR text_match(body,'beta')").rows[0]["n"], Value::Int(2));
        sql(&reopened, "CREATE INDEX another_text ON p USING gin(body); BEGIN; SAVEPOINT retained; DROP INDEX root_text, another_text, root_vector; ROLLBACK TO retained; COMMIT");
        assert_eq!(
            sql(
                &reopened,
                "SELECT count(*) AS n FROM c WHERE text_match(body,'alpha')"
            )
            .rows[0]["n"],
            Value::Int(1)
        );
        sql(&reopened, "DROP INDEX root_text, another_text, root_vector");
        for table in ["p", "c", "d"] {
            assert!(sql(
                &reopened,
                &format!("SELECT * FROM fts_index_stats('{table}')")
            )
            .rows
            .is_empty());
            assert!(reopened
                .storage
                .backend
                .as_ref()
                .unwrap()
                .vector_index(
                    &format!("public.{table}"),
                    "embedding",
                    2,
                    VectorIndexSpec::HNSW(HNSWIndexParams::default()),
                    VectorIndexOpenMode::Restore
                )
                .is_err());
        }
        drop(reopened);
        assert!(Engine::from_persistent_provider(factory)
            .unwrap()
            .list_catalog_indexes()
            .unwrap()
            .is_empty());
    }
}

#[test]
fn partition_column_changes_preserve_then_remove_the_complete_owned_and_expression_graph() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(k int, v text, CONSTRAINT owned UNIQUE(k,v)) PARTITION BY RANGE(k); CREATE TABLE c PARTITION OF p FOR VALUES FROM(0) TO(10); CREATE UNIQUE INDEX expression_root ON p(k,lower(v)); INSERT INTO p VALUES(1,'A')");
        let owned = definition(&first, "owned").catalog;
        sql(&first, "ALTER TABLE p RENAME COLUMN v TO body");
        assert_eq!(definition(&first, "owned").catalog, owned);
        sql(
            &second,
            "INSERT INTO p VALUES(1,'a') ON CONFLICT(k,lower(body)) DO NOTHING",
        );
        sql(&first, "ALTER TABLE p DROP COLUMN body");
        assert!(first.list_catalog_indexes().unwrap().is_empty());
        assert!(first.key_constraints("c").unwrap().is_empty());
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert!(reopened.list_catalog_indexes().unwrap().is_empty());
        assert_eq!(
            sql(&reopened, "SELECT count(*) AS n FROM p").rows[0]["n"],
            Value::Int(1)
        );
    }
}

#[test]
fn temporary_owned_indexes_survive_durable_catalog_refresh_without_persisting() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE TEMP TABLE local_values(k int PRIMARY KEY); INSERT INTO local_values VALUES(1)",
        );
        let owned = definition(&first, "local_values_pkey");
        sql(
            &second,
            "ALTER TABLE t ADD CONSTRAINT durable_key UNIQUE(v)",
        );
        sql(&first, "SELECT * FROM local_values");
        assert_eq!(definition(&first, "local_values_pkey"), owned);
        assert_eq!(
            sql(&first, "SELECT count(*) AS n FROM local_values").rows[0]["n"],
            Value::Int(1)
        );
        assert!(!first
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .load_catalog_indexes()
            .unwrap()
            .iter()
            .any(|row| row.relation.name == "local_values_pkey"));
    }
}
