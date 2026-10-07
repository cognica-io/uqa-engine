//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::cell::Cell;
use uqa_core::RelationIdentity;
use uqa_execution::schema::deletion::{
    perform_deletion, required_address, CatalogRemovalContext, CatalogRemovalInputs,
};
use uqa_execution::schema::table_removal::context::TableRemovalPublication;
use uqa_sql::{schema::removal::hierarchy::HierarchyDropCatalog, SQLError};
use uqa_storage::StorageBackendError;
use uqa_storage::StorageBackendResult;

/// `DROP TABLE public.parent CASCADE` through `context`.
fn drop_parent(context: &CatalogRemovalContext<'_>) -> Result<(), SQLError> {
    perform_deletion(
        context,
        |dependencies| {
            Ok(vec![required_address(
                dependencies.relation_address(&RelationIdentity::new("public", "parent"), None),
                || "table parent".into(),
            )?])
        },
        true,
    )
}

fn assert_recreated_table_undo(engine: &Engine, temporary: bool) {
    let sql = |statement: &str| engine.sql(statement, &[]).unwrap();
    let persistence = if temporary { "TEMP" } else { "" };
    sql(&format!("CREATE {persistence} TABLE replacement_undo(v integer); INSERT INTO replacement_undo VALUES(1)"));
    let original = engine.try_table("replacement_undo").unwrap().unwrap();
    for ending in ["ROLLBACK TO kept; COMMIT", "ROLLBACK"] {
        sql(&format!("BEGIN; SAVEPOINT kept; DROP TABLE replacement_undo; CREATE {persistence} TABLE replacement_undo(other text); INSERT INTO replacement_undo VALUES('replacement')"));
        assert_ne!(
            original.object_id(),
            engine
                .try_table("replacement_undo")
                .unwrap()
                .unwrap()
                .object_id()
        );
        sql(ending);
        let restored = engine.try_table("replacement_undo").unwrap().unwrap();
        assert!(std::sync::Arc::ptr_eq(&original, &restored));
        assert_eq!(
            sql("SELECT v FROM replacement_undo").rows[0]["v"],
            uqa_core::Value::Int(1)
        );
    }
}

#[test]
fn rollback_restores_original_table_incarnation_after_same_name_recreation() {
    assert_recreated_table_undo(&Engine::new(), false);
    for provider in 0..3 {
        let (_directory, engine, _peer) = crate::tests::relation_lock_support::sessions(provider);
        assert_recreated_table_undo(&engine, true);
    }
}

#[test]
fn hierarchy_inputs_retain_the_actual_registry_and_table_metadata_guards() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE parent(id integer); CREATE TABLE child() INHERITS(parent)",
            &[],
        )
        .unwrap();
    let child_id = RelationIdentity::new("public", "child");
    let child = engine.storage.tables.read()[&child_id].clone();
    let tables = HierarchyDropCatalog::tables(&engine);
    assert!(engine.storage.tables.is_locked());
    {
        let (_, child_metadata) = tables.iter().find(|(name, _)| *name == &child_id).unwrap();
        let hierarchy = child_metadata.hierarchy();
        assert!(engine.storage.tables.is_locked());
        assert!(child.hierarchy.is_locked());
        assert_eq!(hierarchy.parents, ["public.parent"]);
    }
    assert!(!child.hierarchy.is_locked());
    drop(tables);
    assert!(!engine.storage.tables.is_locked());
    let (targets, blockers) = engine
        .table_removal_context()
        .hierarchy_drop_targets(&["public.parent".into()], true);
    assert_eq!(targets, ["public.child", "public.parent"]);
    assert_eq!(blockers.len(), 0);
}
/// Fails the table's own removal, which follows the removal of every object that depends on it, as `deleteObjectsInList` orders them.
struct FailedRemoval<'a> {
    engine: &'a Engine,
    children: &'a [&'a str],
    reached: Cell<bool>,
}
impl TableRemovalPublication for FailedRemoval<'_> {
    fn remove_state(&self, name: &str, _: &RelationIdentity) -> StorageBackendResult<()> {
        assert_eq!(name, "public.parent");
        for child in self.children {
            assert_eq!(self.engine.try_foreign_keys(child).unwrap().len(), 0);
        }
        assert!(!self
            .engine
            .durable
            .views
            .read()
            .contains_key(&RelationIdentity::new("public", "dependent")));
        assert!(
            self.engine
                .sequence_state("parent_id_seq")
                .unwrap()
                .is_none(),
            "the sequence owned by a column goes before the column's table"
        );
        assert!(self.engine.has_table("parent").unwrap());
        self.reached.set(true);
        Err(StorageBackendError::Other(
            "injected physical table removal failure".into(),
        ))
    }
    fn prune_constraint_modes(&self) -> Result<(), SQLError> {
        panic!("constraint cleanup must follow successful table removal")
    }
}
fn assert_parent_restored(engine: &Engine, children: &[&str]) {
    assert!(engine.has_table("parent").unwrap());
    for child in children {
        let keys = engine.try_foreign_keys(child).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].ref_table, "public.parent");
    }
    assert!(engine
        .durable
        .views
        .read()
        .contains_key(&RelationIdentity::new("public", "dependent")));
    assert!(engine.sequence_state("parent_id_seq").unwrap().is_some());
}
#[test]
fn failed_table_removal_rolls_back_every_object_removed_before_it() {
    for children in [&["child"][..], &["child_a", "child_b"][..]] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("table-removal.db");
        let engine = Engine::open(&path).unwrap();
        engine
            .sql(
                "CREATE TABLE parent(id serial PRIMARY KEY); CREATE VIEW dependent AS SELECT id FROM parent",
                &[],
            )
            .unwrap();
        for child in children {
            engine
                .sql(
                    &format!(
                        "CREATE TABLE {child}(id integer, FOREIGN KEY(id) REFERENCES parent(id))"
                    ),
                    &[],
                )
                .unwrap();
        }
        let publication = FailedRemoval {
            engine: &engine,
            children,
            reached: Cell::new(false),
        };
        let error = engine
            .with_implicit_transaction(|engine| {
                let mut context = engine.catalog_removal_context();
                context.tables.publication = &publication;
                drop_parent(&context)
            })
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("injected physical table removal failure"));
        assert!(publication.reached.get());
        assert_parent_restored(&engine, children);
        assert_eq!(engine.take_sql_notices().len(), 0);
        drop(engine);
        assert_parent_restored(&Engine::open(&path).unwrap(), children);
    }
}
