//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_core::RelationIdentity;
use uqa_sql::schema::sequences::implicit_ownership::StoredSequenceNames;
use uqa_storage::SequenceOwnerDependency;

fn foreign_table(engine: &Engine, declaration: &str) {
    engine
        .sql(
            "CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw OPTIONS(kind 'memory')",
            &[],
        )
        .unwrap();
    engine
        .sql(
            &format!("CREATE FOREIGN TABLE foreign_items({declaration}) SERVER remote"),
            &[],
        )
        .unwrap();
}
struct GuardObserver<'a> {
    engine: &'a Engine,
    table: Arc<TableState>,
    ordinary_reads: Cell<usize>,
    foreign_reads: Cell<usize>,
}
impl StoredSequenceNames for GuardObserver<'_> {
    fn stored_sequence_name(&self, reference: &str) -> Result<String, String> {
        StoredSequenceNames::stored_sequence_name(self.engine, reference)
    }
}
impl SequenceExpressionCatalog for GuardObserver<'_> {
    fn object_ids(&self) -> SequenceExpressionObjectIdsRead<'_> {
        if self.table.columns.is_locked() {
            assert!(
                self.table.table_checks.is_locked(),
                "both actual table metadata guards must survive expression analysis"
            );
            assert!(!self.engine.durable.foreign_tables.is_locked());
            self.ordinary_reads.set(self.ordinary_reads.get() + 1);
        } else {
            assert!(!self.table.table_checks.is_locked());
            assert!(
                self.engine.durable.foreign_tables.is_locked(),
                "foreign analysis must retain the real registry guard"
            );
            self.foreign_reads.set(self.foreign_reads.get() + 1);
        }
        SequenceExpressionCatalog::object_ids(self.engine)
    }
}
#[test]
fn dependency_analysis_retains_actual_table_and_foreign_guards_until_expression_reads_finish() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SEQUENCE ids; CREATE TABLE items(value bigint DEFAULT nextval('ids'))",
            &[],
        )
        .unwrap();
    foreign_table(&engine, "value bigint DEFAULT nextval('ids')");
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
    let observer = GuardObserver {
        engine: &engine,
        table: table.clone(),
        ordinary_reads: Cell::new(0),
        foreign_reads: Cell::new(0),
    };
    let mut context = engine.sequence_dependency_context();
    context.expressions = &observer;
    let dependents = context
        .sequence_schema_expression_dependents("public.ids")
        .unwrap();
    assert_eq!(dependents.len(), 2);
    assert!(dependents.contains(&SequenceSchemaDependent::Default {
        table: "public.items".into(),
        column: "value".into(),
        foreign: false
    }));
    assert!(dependents.contains(&SequenceSchemaDependent::Default {
        table: "public.foreign_items".into(),
        column: "value".into(),
        foreign: true
    }));
    assert_eq!(observer.ordinary_reads.get(), 1);
    assert_eq!(observer.foreign_reads.get(), 1);
    assert!(!table.columns.is_locked());
    assert!(!table.table_checks.is_locked());
    assert!(!engine.durable.foreign_tables.is_locked());
    let identities = SequenceExpressionCatalog::object_ids(&engine);
    assert!(engine.durable.sequence_object_ids.is_locked());
    drop(identities);
    assert!(!engine.durable.sequence_object_ids.is_locked());
}
struct CountedCatalog<'a> {
    engine: &'a Engine,
    foreign_reads: Cell<usize>,
}
impl SequenceDependencyCatalog for CountedCatalog<'_> {
    fn refresh_tables(&self) -> StorageBackendResult<()> {
        SequenceDependencyCatalog::refresh_tables(self.engine)
    }
    fn refresh_catalog(&self) -> StorageBackendResult<()> {
        SequenceDependencyCatalog::refresh_catalog(self.engine)
    }
    fn table_entries(&self) -> Vec<(String, Arc<dyn SequenceTableMetadata>)> {
        SequenceDependencyCatalog::table_entries(self.engine)
    }
    fn resolve_table_name(&self, name: &str) -> StorageBackendResult<Option<String>> {
        SequenceDependencyCatalog::resolve_table_name(self.engine, name)
    }
    fn table(&self, name: &str) -> StorageBackendResult<Option<Arc<dyn SequenceTableMetadata>>> {
        SequenceDependencyCatalog::table(self.engine, name)
    }
    fn foreign_tables(&self) -> ForeignTablesRead<'_> {
        self.foreign_reads.set(self.foreign_reads.get() + 1);
        SequenceDependencyCatalog::foreign_tables(self.engine)
    }
}
#[test]
fn owner_identity_lookup_reads_foreign_metadata_only_after_live_ordinary_tables_miss() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE items(id serial)", &[]).unwrap();
    foreign_table(&engine, "id serial");
    let ordinary = engine
        .sequence_state("items_id_seq")
        .unwrap()
        .unwrap()
        .1
        .owner
        .unwrap();
    let foreign = engine
        .sequence_state("foreign_items_id_seq")
        .unwrap()
        .unwrap()
        .1
        .owner
        .unwrap();
    let catalog = CountedCatalog {
        engine: &engine,
        foreign_reads: Cell::new(0),
    };
    let mut context = engine.sequence_dependency_context();
    context.catalog = &catalog;
    assert_eq!(
        context.sequence_owner_target(ordinary),
        Some(("public.items".into(), "id".into(), false))
    );
    assert_eq!(catalog.foreign_reads.get(), 0);
    assert_eq!(
        context.sequence_owner_target(foreign),
        Some(("public.foreign_items".into(), "id".into(), true))
    );
    assert_eq!(catalog.foreign_reads.get(), 1);
    assert!(context
        .sequence_owner_target(SequenceOwner {
            table_object_id: [9; 16],
            column_object_id: [8; 16],
            dependency: SequenceOwnerDependency::Automatic
        })
        .is_none());
    assert_eq!(catalog.foreign_reads.get(), 2);
}
fn schemas(engine: &Engine) -> serde_json::Value {
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
    let columns = table.columns.read().clone();
    let checks = table.table_checks.read().clone();
    let foreign = engine.durable.foreign_tables.read()
        [&RelationIdentity::new("public", "foreign_items")]
        .columns
        .clone();
    serde_json::to_value((columns, checks, foreign)).unwrap()
}
fn assert_detached(engine: &Engine) {
    let table = engine.storage.tables.read()[&RelationIdentity::new("public", "items")].clone();
    let columns = table.columns.read();
    assert_eq!(
        columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "kept"]
    );
    assert!(columns[0].default.is_none());
    assert!(columns[0].auto_increment.is_none());
    assert!(columns[1].default.is_some());
    assert!(table.table_checks.read().is_empty());
    assert!(engine.durable.foreign_tables.read()
        [&RelationIdentity::new("public", "foreign_items")]
        .columns[0]
        .default
        .is_none());
    assert!(engine.sequence_state("items_id_seq").unwrap().is_none());
}
#[test]
fn native_sequence_cascade_restores_real_metadata_on_rollback_and_persists_detachment_on_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dependencies.db");
    let engine = Engine::open(&path).unwrap();
    engine.sql("CREATE TABLE items(id serial, kept integer DEFAULT 7); INSERT INTO items(kept) VALUES(8); ALTER TABLE items ADD COLUMN reference regclass GENERATED ALWAYS AS ('items_id_seq'::regclass) STORED; ALTER TABLE items ADD CONSTRAINT seq_check CHECK(nextval('items_id_seq') > 0)",&[]).unwrap();
    foreign_table(&engine, "value bigint DEFAULT nextval('items_id_seq')");
    let before = schemas(&engine);
    let restricted = engine
        .sql("DROP SEQUENCE items_id_seq RESTRICT", &[])
        .unwrap_err();
    assert_eq!(restricted.sqlstate(), Some("2BP01"));
    assert_eq!(schemas(&engine), before);
    engine
        .sql("BEGIN; DROP SEQUENCE items_id_seq CASCADE", &[])
        .unwrap();
    assert_detached(&engine);
    let after = schemas(&engine);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(schemas(&engine), before);
    assert!(engine.sequence_state("items_id_seq").unwrap().is_some());
    engine
        .sql("DROP SEQUENCE items_id_seq CASCADE", &[])
        .unwrap();
    assert_detached(&engine);
    assert_eq!(schemas(&engine), after);
    drop(engine);
    let reopened = Engine::open(&path).unwrap();
    assert_detached(&reopened);
    assert_eq!(schemas(&reopened), after);
    assert_eq!(
        reopened.sql("SELECT kept FROM items", &[]).unwrap().rows[0]["kept"],
        uqa_core::Value::Int(8)
    );
}
