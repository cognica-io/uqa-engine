//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored index ownership, hierarchy and dependency lifetimes over persistent sessions.

use crate::tests::relation_lock_support::{error, sessions, sql};
use crate::Engine;
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::catalog::index::IndexDefinition;

mod physical;
mod renaming;
mod restoration;

fn definition(engine: &Engine, name: &str) -> IndexDefinition {
    crate::catalog_indexes::index_definition(&engine.catalog_index(name).unwrap().unwrap()).unwrap()
}

#[test]
fn owned_index_addresses_follow_constraint_rename_undo_recreation_and_reopen() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "ALTER TABLE t ADD CONSTRAINT owned UNIQUE(v)");
        let original = definition(&first, "owned");
        let index = original.catalog.as_ref().unwrap();
        let key = first.key_constraints("t").unwrap().remove(0);
        let owner = key.catalog_identity.unwrap();
        assert_eq!(
            original.relationships.owning_constraint,
            Some(owner.object_id)
        );
        assert_ne!(index.identity.object_id, owner.object_id);
        let row = sql(
            &first,
            "SELECT conindid FROM pg_constraint WHERE conname = 'owned'",
        );
        assert_eq!(row.rows[0]["conindid"], Value::Int(index.identity.oid));
        sql(
            &first,
            "BEGIN; SAVEPOINT retained; ALTER TABLE t RENAME CONSTRAINT owned TO renamed",
        );
        assert!(first.catalog_index("owned").unwrap().is_none());
        assert_eq!(definition(&first, "renamed"), original);
        sql(&first, "ROLLBACK TO retained; ALTER TABLE t DROP CONSTRAINT owned; ALTER TABLE t ADD CONSTRAINT owned UNIQUE(v)");
        let replacement = definition(&first, "owned");
        assert_ne!(replacement.catalog, original.catalog);
        assert_ne!(
            replacement.relationships.owning_constraint,
            original.relationships.owning_constraint
        );
        sql(&first, "ROLLBACK TO retained; COMMIT");
        assert_eq!(definition(&second, "owned"), original);
        error(&second, "INSERT INTO t VALUES(1)", "23505");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(definition(&reopened, "owned"), original);
        sql(&reopened, "ALTER TABLE t DROP CONSTRAINT owned");
        assert!(reopened.catalog_index("owned").unwrap().is_none());
        sql(&reopened, "INSERT INTO t VALUES(1)");
    }
}

#[test]
fn partition_owned_indexes_preserve_addresses_on_detach_and_reuse_only_owned_keys() {
    for provider in 0..3 {
        let (_directory, first, _) = sessions(provider);
        sql(&first, "CREATE TABLE p(k int CONSTRAINT root_key UNIQUE) PARTITION BY RANGE(k); CREATE TABLE c(k int); CREATE UNIQUE INDEX local_idx ON c(k); ALTER TABLE p ATTACH PARTITION c FOR VALUES FROM(0) TO(10)");
        let root = definition(&first, "root_key").catalog.unwrap();
        let child = first.key_constraints("c").unwrap().remove(0);
        let name = child.name.as_ref().unwrap();
        let attached = definition(&first, name);
        assert_eq!(
            attached.relationships.parent_index,
            Some(root.identity.object_id)
        );
        assert!(definition(&first, "local_idx")
            .relationships
            .parent_index
            .is_none());
        assert_eq!(
            sql(
                &first,
                "SELECT count(*) AS n FROM pg_index WHERE indrelid='c'::regclass"
            )
            .rows[0]["n"],
            Value::Int(2)
        );
        assert_eq!(sql(&first, "SELECT count(*) AS n FROM pg_constraint c JOIN pg_constraint p ON p.oid=c.conparentid WHERE p.conname='root_key'").rows[0]["n"], Value::Int(1));
        sql(&first, "ALTER TABLE p DETACH PARTITION c");
        let detached = definition(&first, name);
        assert_eq!(detached.catalog, attached.catalog);
        assert!(detached.relationships.parent_index.is_none());
        sql(
            &first,
            "ALTER TABLE p ATTACH PARTITION c FOR VALUES FROM(0) TO(10)",
        );
        assert_eq!(definition(&first, name), attached);
        sql(&first, "ALTER TABLE p DROP CONSTRAINT root_key");
        assert!(first.key_constraints("c").unwrap().is_empty());
        assert!(first.catalog_index(name).unwrap().is_none());
        assert!(first.catalog_index("local_idx").unwrap().is_some());
    }
}

#[test]
fn independent_partition_index_can_reuse_a_primary_key_and_requires_cascade_to_remove_it() {
    for provider in 0..3 {
        let (_directory, first, _) = sessions(provider);
        sql(&first, "CREATE TABLE p(k int) PARTITION BY RANGE(k); CREATE UNIQUE INDEX root_idx ON p(k); CREATE TABLE c(k int CONSTRAINT local_key PRIMARY KEY)");
        let before = definition(&first, "local_key").catalog;
        sql(
            &first,
            "ALTER TABLE p ATTACH PARTITION c FOR VALUES FROM(0) TO(10)",
        );
        assert_eq!(definition(&first, "local_key").catalog, before);
        assert_eq!(
            sql(
                &first,
                "SELECT count(*) AS n FROM pg_index WHERE indrelid='c'::regclass"
            )
            .rows[0]["n"],
            Value::Int(1)
        );
        assert_eq!(
            sql(
                &first,
                "SELECT conparentid FROM pg_constraint WHERE conname='local_key'"
            )
            .rows[0]["conparentid"],
            Value::Int(0)
        );
        error(&first, "DROP INDEX root_idx", "2BP01");
        assert!(first.drop_catalog_index("root_idx").is_err());
        assert_eq!(definition(&first, "local_key").catalog, before);
        sql(&first, "DROP INDEX root_idx CASCADE");
        assert!(first.key_constraints("c").unwrap().is_empty());
        assert!(first.catalog_index("local_key").unwrap().is_none());
    }
}

#[test]
fn partition_expression_arbiters_use_local_physical_keys_after_reopen() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(k int, v text) PARTITION BY RANGE(k); CREATE TABLE c PARTITION OF p FOR VALUES FROM(0) TO(10); CREATE UNIQUE INDEX expression_root ON p(k, lower(v)); INSERT INTO p VALUES(1,'A')");
        let root = definition(&first, "expression_root").catalog.unwrap();
        let child = first
            .durable
            .catalog_indexes
            .read()
            .values()
            .find(|row| row.table_name == "public.c")
            .unwrap()
            .relation
            .name
            .clone();
        let local = definition(&first, &child).catalog.unwrap();
        assert_ne!(local, root);
        sql(
            &first,
            "INSERT INTO p VALUES(1,'a') ON CONFLICT(k,lower(v)) DO NOTHING",
        );
        error(&second, "INSERT INTO c VALUES(1,'a')", "23505");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(definition(&reopened, &child).catalog, Some(local));
        sql(
            &reopened,
            "INSERT INTO p VALUES(1,'a') ON CONFLICT(k,lower(v)) DO NOTHING",
        );
        assert_eq!(
            sql(&reopened, "SELECT count(*) AS n FROM p").rows[0]["n"],
            Value::Int(1)
        );
    }
}

#[test]
fn adding_partition_key_materializes_descendants_and_renamed_foreign_key_dependencies() {
    for provider in 0..3 {
        let (_directory, first, _) = sessions(provider);
        sql(&first, "CREATE TABLE p(k int) PARTITION BY RANGE(k); CREATE TABLE c PARTITION OF p FOR VALUES FROM(0) TO(10); ALTER TABLE p ADD CONSTRAINT root_key UNIQUE(k); CREATE TABLE ref(k int REFERENCES p(k)); INSERT INTO p VALUES(1); INSERT INTO ref VALUES(1)");
        assert_eq!(first.key_constraints("c").unwrap().len(), 1);
        let root = definition(&first, "root_key").catalog.unwrap();
        let foreign = first.foreign_keys("ref").unwrap().remove(0);
        assert_eq!(foreign.referenced_index, Some(root.identity.object_id));
        sql(
            &first,
            "ALTER TABLE p RENAME CONSTRAINT root_key TO renamed",
        );
        assert_eq!(
            sql(
                &first,
                "SELECT conindid FROM pg_constraint WHERE conrelid='ref'::regclass AND contype='f'"
            )
            .rows[0]["conindid"],
            Value::Int(root.identity.oid)
        );
        error(&first, "ALTER TABLE p DROP CONSTRAINT renamed", "2BP01");
        sql(&first, "ALTER TABLE p DROP CONSTRAINT renamed CASCADE");
        assert!(first.key_constraints("c").unwrap().is_empty());
        assert!(first.foreign_keys("ref").unwrap().is_empty());
    }
}

#[test]
fn equivalent_owned_parents_and_an_independent_parent_keep_distinct_child_indexes() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE TABLE p(k int) PARTITION BY RANGE(k); CREATE UNIQUE INDEX independent ON p(k); CREATE TABLE c(k int CONSTRAINT local_key UNIQUE); ALTER TABLE p ATTACH PARTITION c FOR VALUES FROM(0) TO(10); ALTER TABLE p ADD CONSTRAINT owned_a UNIQUE(k); ALTER TABLE p ADD CONSTRAINT owned_b UNIQUE(k)");
        assert_eq!(first.key_constraints("c").unwrap().len(), 3);
        assert_eq!(sql(&first, "SELECT count(*) AS n FROM pg_inherits i JOIN pg_class c ON c.oid=i.inhrelid WHERE c.relkind='i'").rows[0]["n"], Value::Int(3));
        sql(&first, "ALTER TABLE p DROP CONSTRAINT owned_a");
        assert_eq!(first.key_constraints("c").unwrap().len(), 2);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        drop((first, second));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(reopened.key_constraints("c").unwrap().len(), 2);
        sql(
            &reopened,
            "ALTER TABLE p DROP CONSTRAINT owned_b; DROP INDEX independent CASCADE",
        );
        assert!(reopened.key_constraints("c").unwrap().is_empty());
    }
}
