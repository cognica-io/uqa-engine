//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Initial conversion and current-metadata rejection through real provider sessions.

use super::*;
use uqa_sql::ast::{ColumnDef, TableConstraintSet};
use uqa_storage::ValueIndexKey;

#[test]
fn legacy_derived_search_indexes_build_once_during_the_initial_conversion() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(k int, body text, embedding vector(2)) PARTITION BY RANGE(k); CREATE TABLE c PARTITION OF p FOR VALUES FROM(0) TO(10); INSERT INTO p VALUES(1,'alpha',ARRAY[1.0,0.0]); CREATE INDEX text_root ON p USING gin(body); CREATE INDEX vector_root ON p USING hnsw(embedding)");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        for row in raw.catalog.load_catalog_indexes().unwrap() {
            if row.table_name == "public.c" {
                raw.catalog.drop_catalog_index(&row.relation).unwrap();
            }
        }
        raw.backend
            .drop_vector_index_metadata("public.c", "embedding")
            .unwrap();
        raw.catalog
            .drop_table_field_analyzer_field("public.c", "body")
            .unwrap();
        let mut child = raw
            .catalog
            .load_tables()
            .unwrap()
            .into_iter()
            .find(|row| row.relation.name == "c")
            .unwrap();
        child.fts_fields.clear();
        raw.catalog.save_table(&child).unwrap();
        raw.catalog
            .delete_metadata("sql_index_registry_version")
            .unwrap();
        drop((first, second));
        let restored = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        assert_eq!(
            sql(
                &restored,
                "SELECT count(*) AS n FROM c WHERE text_match(body,'alpha')"
            )
            .rows[0]["n"],
            Value::Int(1)
        );
        assert_eq!(
            restored
                .knn_search("c", "embedding", vec![1.0, 0.0], 1)
                .unwrap()
                .len(),
            1
        );
        drop(restored);
        raw.backend
            .drop_vector_index_metadata("public.c", "embedding")
            .unwrap();
        assert!(Engine::from_persistent_provider(factory).is_err());
    }
}

#[test]
fn legacy_partition_registry_preserves_postings_addresses_and_foreign_key_targets() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(k int CONSTRAINT root_key UNIQUE, v text) PARTITION BY RANGE(k); CREATE TABLE c PARTITION OF p FOR VALUES FROM(0) TO(10); CREATE UNIQUE INDEX expression_root ON p(k,lower(v)); CREATE TABLE ref(k int REFERENCES p(k)); INSERT INTO p VALUES(1,'A'); INSERT INTO ref VALUES(1)");
        let root = definition(&first, "expression_root").catalog.unwrap();
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut postings = None;
        for row in raw.catalog.load_catalog_indexes().unwrap() {
            let index = crate::catalog_indexes::index_definition(&row).unwrap();
            if row.table_name == "public.c" && index.relationships.owning_constraint.is_none() {
                let local =
                    ValueIndexKey::Index(index.catalog.as_ref().unwrap().physical_key.clone());
                let values = raw
                    .backend
                    .load_btree_index("public.c", &local)
                    .unwrap()
                    .unwrap();
                let predecessor = ValueIndexKey::Index(root.physical_key.clone());
                raw.backend
                    .replace_btree_indexes("public.c", &[(&predecessor, &values)])
                    .unwrap();
                raw.backend.drop_btree_index("public.c", &local).unwrap();
                postings = Some(values);
            }
            if index.relationships.owning_constraint.is_some()
                || index.relationships.parent_index.is_some()
            {
                raw.catalog.drop_catalog_index(&row.relation).unwrap();
            }
        }
        for mut row in raw.catalog.load_tables().unwrap() {
            let mut columns: Vec<ColumnDef> = serde_json::from_str(&row.columns_json).unwrap();
            let mut constraints: TableConstraintSet =
                serde_json::from_str(&row.constraints_json).unwrap();
            for column in &mut columns {
                if let Some(reference) = &mut column.references {
                    reference.referenced_index = None;
                }
            }
            for key in constraints
                .foreign_keys
                .iter_mut()
                .chain(&mut constraints.hierarchy.partition_inherited_foreign_keys)
            {
                key.referenced_index = None;
            }
            row.columns_json = serde_json::to_string(&columns).unwrap();
            row.constraints_json = serde_json::to_string(&constraints).unwrap();
            raw.catalog.save_table(&row).unwrap();
        }
        raw.catalog
            .delete_metadata("sql_index_registry_version")
            .unwrap();
        drop((first, second));
        let restored = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        assert_eq!(
            definition(&restored, "expression_root").catalog,
            Some(root.clone())
        );
        let owned = definition(&restored, "root_key").catalog.unwrap();
        assert_eq!(
            owned.identity.oid,
            uqa_sql::catalog::oids::relation_oid("I", "public", "root_key")
        );
        assert_eq!(
            restored.foreign_keys("ref").unwrap()[0].referenced_index,
            Some(owned.identity.object_id)
        );
        assert_eq!(
            raw.backend
                .load_btree_index("public.c", &ValueIndexKey::Index(root.physical_key))
                .unwrap(),
            postings
        );
        sql(
            &restored,
            "INSERT INTO p VALUES(1,'a') ON CONFLICT(k,lower(v)) DO NOTHING",
        );
        error(&restored, "DELETE FROM p WHERE k=1", "23503");
        sql(
            &restored,
            "ALTER TABLE p RENAME CONSTRAINT root_key TO renamed",
        );
        error(&restored, "ALTER TABLE p DROP CONSTRAINT renamed", "2BP01");
        drop(restored);
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(definition(&reopened, "renamed").catalog, Some(owned));
    }
}

#[test]
fn malformed_current_ownership_and_parent_edges_are_rejected_without_repair() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(k int CONSTRAINT root_key UNIQUE) PARTITION BY RANGE(k); CREATE TABLE c PARTITION OF p FOR VALUES FROM(0) TO(10)");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let valid = raw
            .catalog
            .load_catalog_indexes()
            .unwrap()
            .into_iter()
            .find(|row| row.table_name == "public.c")
            .unwrap();
        drop((first, second));
        for damage in 0..5 {
            let mut damaged = valid.clone();
            let mut index = crate::catalog_indexes::index_definition(&damaged).unwrap();
            match damage {
                0 => index.relationships.owning_constraint = None,
                1 => index.relationships.owning_constraint = Some([99; 16]),
                2 => index.relationships.parent_index = None,
                3 => index.relationships.parent_index = Some([99; 16]),
                4 => {
                    index.relationships.parent_index =
                        Some(index.catalog.as_ref().unwrap().identity.object_id);
                }
                _ => unreachable!(),
            }
            damaged.definition_json = Some(serde_json::to_string(&index).unwrap());
            raw.catalog.save_catalog_index_row(&damaged).unwrap();
            assert!(
                Engine::from_persistent_provider(Arc::clone(&factory)).is_err(),
                "accepted damage {damage}"
            );
            let retained = raw
                .catalog
                .load_catalog_indexes()
                .unwrap()
                .into_iter()
                .find(|row| row.relation == damaged.relation)
                .unwrap();
            assert_eq!(retained.definition_json, damaged.definition_json);
        }
        raw.catalog.save_catalog_index_row(&valid).unwrap();
        assert!(Engine::from_persistent_provider(factory).is_ok());
    }
}

#[test]
fn later_hydration_failure_restores_the_entire_legacy_registry_conversion() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(
            &first,
            "ALTER TABLE t ADD CONSTRAINT owned UNIQUE(v); CREATE INDEX bad_vector ON t(v)",
        );
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        for mut row in raw.catalog.load_catalog_indexes().unwrap() {
            if row.relation.name == "owned" {
                raw.catalog.drop_catalog_index(&row.relation).unwrap();
            } else {
                row.index_type = "ivf".into();
                raw.catalog.save_catalog_index_row(&row).unwrap();
            }
        }
        raw.catalog
            .delete_metadata("sql_index_registry_version")
            .unwrap();
        drop((first, second));
        let failure = Engine::from_persistent_provider(Arc::clone(&factory))
            .err()
            .expect("invalid vector index accepted");
        assert!(failure.to_string().contains("non-vector"), "{failure}");
        assert!(raw
            .catalog
            .get_metadata("sql_index_registry_version")
            .unwrap()
            .is_none());
        let mut rows = raw.catalog.load_catalog_indexes().unwrap();
        assert_eq!(rows.len(), 1);
        rows[0].index_type = "btree".into();
        raw.catalog.save_catalog_index_row(&rows[0]).unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        error(&restored, "INSERT INTO t VALUES(1)", "23505");
    }
}
