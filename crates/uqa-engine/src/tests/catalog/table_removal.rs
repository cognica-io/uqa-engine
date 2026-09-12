//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use std::cell::Cell;
use uqa_core::RelationIdentity;
use uqa_execution::schema::table_removal::context::{
    TableChecksWrite, TableColumnsWrite, TableForeignKeysWrite, TableRemovalCatalog,
    TableRemovalEntry, TableRemovalPublication, TableRemovalState,
};
use uqa_sql::{
    ast::{ColumnDef, ForeignKey, TableCheck, TableKeyConstraint},
    schema::removal::{
        hierarchy::HierarchyDropCatalog,
        tables::{
            TableChecksRead, TableColumnsRead, TableForeignKeysRead, TableKeysRead,
            TableRemovalMetadata,
        },
    },
    SQLError,
};
use uqa_storage::StorageBackendError;
use uqa_storage::StorageBackendResult;
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
    assert!(blockers.is_empty());
}
struct FailedRemoval<'a> {
    engine: &'a Engine,
    reached: Cell<bool>,
}
impl TableRemovalPublication for FailedRemoval<'_> {
    fn remove_state(&self, name: &str, _: &RelationIdentity) -> StorageBackendResult<()> {
        assert_eq!(name, "public.parent");
        assert!(self.engine.try_foreign_keys("child").unwrap().is_empty());
        assert!(!self
            .engine
            .durable
            .views
            .read()
            .contains_key(&RelationIdentity::new("public", "dependent")));
        assert!(self.engine.has_table("parent").unwrap());
        assert!(self
            .engine
            .sequence_state("parent_id_seq")
            .unwrap()
            .is_some());
        self.reached.set(true);
        Err(StorageBackendError::Other(
            "injected physical table removal failure".into(),
        ))
    }
    fn prune_constraint_modes(&self) -> Result<(), SQLError> {
        panic!("constraint cleanup must follow successful table publication")
    }
}
fn assert_parent_restored(engine: &Engine) {
    assert!(engine.has_table("parent").unwrap());
    assert!(!engine.try_foreign_keys("child").unwrap().is_empty());
    assert!(engine
        .durable
        .views
        .read()
        .contains_key(&RelationIdentity::new("public", "dependent")));
    assert!(engine.sequence_state("parent_id_seq").unwrap().is_some());
}
#[test]
fn failed_physical_removal_rolls_back_cascaded_views_and_foreign_keys_before_owned_sequence_drop() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("table-removal.db");
    let engine = Engine::open(&path).unwrap();
    engine.sql("CREATE TABLE parent(id serial PRIMARY KEY); CREATE TABLE child(id integer, CONSTRAINT child_parent FOREIGN KEY(id) REFERENCES parent(id)); CREATE VIEW dependent AS SELECT id FROM parent", &[]).unwrap();
    let publication = FailedRemoval {
        engine: &engine,
        reached: Cell::new(false),
    };
    let error = engine
        .with_implicit_storage_transaction(|engine| {
            let mut context = engine.table_removal_context();
            context.publication = &publication;
            context.try_drop_tables_inner(&["public.parent".into()], true)
        })
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected physical table removal failure"));
    assert!(publication.reached.get());
    assert_parent_restored(&engine);
    drop(publication);
    drop(engine);
    assert_parent_restored(&Engine::open(&path).unwrap());
}
struct FailedCandidateCatalog<'a> {
    engine: &'a Engine,
    writes: Cell<usize>,
}
struct Candidate<'a> {
    inner: Box<dyn TableRemovalState + 'a>,
    observer: &'a FailedCandidateCatalog<'a>,
}
impl TableRemovalMetadata for Candidate<'_> {
    fn columns(&self) -> TableColumnsRead<'_> {
        self.inner.columns()
    }
    fn table_checks(&self) -> TableChecksRead<'_> {
        self.inner.table_checks()
    }
    fn foreign_keys(&self) -> TableForeignKeysRead<'_> {
        self.inner.foreign_keys()
    }
    fn key_constraints(&self) -> TableKeysRead<'_> {
        self.inner.key_constraints()
    }
}
impl TableRemovalState for Candidate<'_> {
    fn object_id(&self) -> [u8; 16] {
        self.inner.object_id()
    }
    fn columns_write(&self) -> TableColumnsWrite<'_> {
        self.inner.columns_write()
    }
    fn checks_write(&self) -> TableChecksWrite<'_> {
        self.inner.checks_write()
    }
    fn foreign_keys_write(&self) -> TableForeignKeysWrite<'_> {
        self.inner.foreign_keys_write()
    }
    fn persist_constraints(
        &self,
        columns: &[ColumnDef],
        checks: &[TableCheck],
        foreign_keys: &[ForeignKey],
        keys: &[TableKeyConstraint],
    ) -> StorageBackendResult<()> {
        for name in ["child_a", "child_b"] {
            let identity = RelationIdentity::new("public", name);
            let table = self.observer.engine.storage.tables.read()[&identity].clone();
            assert!(
                !table.foreign_keys.read().is_empty(),
                "no candidate may publish before all persistence succeeds"
            );
        }
        let writes = self.observer.writes.get() + 1;
        self.observer.writes.set(writes);
        if writes == 2 {
            return Err(StorageBackendError::Other(
                "injected second schema persistence failure".into(),
            ));
        }
        self.inner
            .persist_constraints(columns, checks, foreign_keys, keys)
    }
}
impl TableRemovalCatalog for FailedCandidateCatalog<'_> {
    fn relation_kind(&self, name: &str) -> StorageBackendResult<Option<(String, &'static str)>> {
        TableRemovalCatalog::relation_kind(self.engine, name)
    }
    fn contains_relation(&self, relation: &RelationIdentity) -> bool {
        TableRemovalCatalog::contains_relation(self.engine, relation)
    }
    fn table_entries(&self) -> Vec<TableRemovalEntry<'_>> {
        TableRemovalCatalog::table_entries(self.engine)
            .into_iter()
            .map(|(name, inner)| {
                (
                    name,
                    Box::new(Candidate {
                        inner,
                        observer: self,
                    }) as Box<dyn TableRemovalState>,
                )
            })
            .collect()
    }
}
fn assert_children_restored(engine: &Engine) {
    assert!(engine.has_table("parent").unwrap());
    for name in ["child_a", "child_b"] {
        let keys = engine.try_foreign_keys(name).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].ref_table, "public.parent");
    }
}
#[test]
fn failed_later_cascade_candidate_preserves_every_live_schema_and_rolls_back_prior_durable_writes()
{
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("table-candidates.db");
    let engine = Engine::open(&path).unwrap();
    engine.sql("CREATE TABLE parent(id integer PRIMARY KEY); CREATE TABLE child_a(id integer, FOREIGN KEY(id) REFERENCES parent(id)); CREATE TABLE child_b(id integer, FOREIGN KEY(id) REFERENCES parent(id))", &[]).unwrap();
    let catalog = FailedCandidateCatalog {
        engine: &engine,
        writes: Cell::new(0),
    };
    let error = engine
        .with_implicit_storage_transaction(|engine| {
            let mut context = engine.table_removal_context();
            context.catalog = &catalog;
            context.try_drop_tables_inner(&["public.parent".into()], true)
        })
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected second schema persistence failure"));
    assert_eq!(catalog.writes.get(), 2);
    assert_children_restored(&engine);
    drop(catalog);
    drop(engine);
    assert_children_restored(&Engine::open(&path).unwrap());
}
