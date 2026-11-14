//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::catalog_role::RoleIdentity;
use uqa_sql::ast::ColumnDef;
use uqa_storage::{
    catalog::{
        input::{table, CatalogInputLimits},
        table_input::CatalogTableInput,
        table_load::{CatalogTableLoad, CatalogTableOrder},
    },
    key_value::{KeyValueRead, KeyValueReadRevision, RecordKeyReadVisitor},
    read_control::{KeyValueReadVisitor, ValueReadVisitor},
    CatalogFacade, KeyValueCatalog, MemoryKeyValueStore, RelationSecurityRow, TableSchema,
};

struct Source {
    rows: Vec<(Vec<u8>, Vec<u8>)>,
    control: StorageReadControl,
}

impl KeyValueRead for Source {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        panic!("view is already retained")
    }
    fn visit_value(&self, _: &[u8], _: &mut ValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("bounded input required")
    }
    fn visit_prefix(&self, _: &[u8], _: &mut KeyValueReadVisitor<'_>) -> StorageBackendResult<()> {
        panic!("bounded input required")
    }
    fn visit_record_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visitor: &mut RecordKeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        assert_eq!(limit, 1);
        if let Some((key, _)) = self.rows.iter().find(|(key, _)| {
            key.starts_with(prefix) && after.is_none_or(|after| key.as_slice() > after)
        }) {
            visitor(key, true)?;
        }
        Ok(())
    }
    fn visit_value_bounded(
        &self,
        key: &[u8],
        maximum: usize,
        control: &StorageReadControl,
        visitor: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let bytes = self
            .rows
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, bytes)| bytes.as_slice());
        if let Some(bytes) = bytes {
            control.check_value_size(bytes.len(), maximum)?;
        }
        visitor(bytes)
    }
}

fn tables(values: &[TableSchema], control: &StorageReadControl) -> CatalogTables {
    let source = Arc::new(Source {
        rows: values
            .iter()
            .enumerate()
            .map(|(i, value)| {
                (
                    format!("d/{i:04}").into_bytes(),
                    serde_json::to_vec(value).unwrap(),
                )
            })
            .collect(),
        control: control.clone(),
    });
    let input = CatalogTableInput::new(
        source,
        (b"d/", |_, bytes, control| {
            table::row(bytes, control).map(Some)
        }),
        (b"a/", |_, _, _| panic!("no independent ACL records")),
        CatalogInputLimits {
            maximum_key_bytes: 4096,
            maximum_value_bytes: 1 << 20,
        },
        control,
    )
    .unwrap();
    let mut load =
        CatalogTableLoad::new(input, CatalogTableOrder::AfterReplacements, None, control).unwrap();
    for _ in 0..4096 {
        if let Some(tables) = load.advance(64).unwrap() {
            return tables;
        }
    }
    panic!("physical table input did not complete")
}

fn schema(name: &str) -> TableSchema {
    let column = ColumnDef::nullable("id", uqa_sql::ColumnType::Integer);
    TableSchema {
        relation: RelationIdentity::new("public", name),
        security: RelationSecurityRow::bootstrap(),
        object_id: [1; 16],
        storage_generation: [2; 16],
        analyzer_json: "uninterpreted analyzer".into(),
        fts_fields: vec!["body".into()],
        vector_fields: vec![VectorFieldSchema {
            field: "embedding".into(),
            dimensions: 3,
        }],
        columns_json: serde_json::to_string(&[column]).unwrap(),
        constraints_json: "uninterpreted constraints".into(),
    }
}

fn input(values: &[TableSchema], control: &StorageReadControl) -> TableDeclarationInput {
    TableDeclarationInput::new(
        tables(values, control),
        RoleDefinitions::initial_catalog(),
        control,
    )
    .unwrap()
}

fn finish(input: &mut TableDeclarationInput) -> StorageBackendResult<PreparedTableDeclarations> {
    for _ in 0..16384 {
        if let Some(tables) = input.advance(1)? {
            return Ok(tables);
        }
    }
    panic!("table declaration input did not complete")
}

#[test]
fn prepared_tables_keep_original_fields_and_native_columns_after_input_retirement() {
    let control = StorageReadControl::with_limit(1 << 20);
    let schemas = [schema("z"), schema("a")];
    let tables = tables(&schemas, &control);
    let pointers = tables
        .iter()
        .map(|table| {
            (
                table.relation.name.as_ptr(),
                table.analyzer_json.as_ptr(),
                table.constraints_json.as_ptr(),
                table.fts_fields.as_ptr(),
                table.vector_fields.as_ptr(),
            )
        })
        .collect::<Vec<_>>();
    let mut input =
        TableDeclarationInput::new(tables, RoleDefinitions::initial_catalog(), &control).unwrap();
    let completed = finish(&mut input).unwrap();
    drop(input);
    control.cancellation().cancel();
    assert_eq!(
        completed
            .iter()
            .map(|table| table.relation.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    for (table, pointers) in completed.iter().zip(pointers) {
        assert_eq!(
            (
                table.relation.name.as_ptr(),
                table.analyzer_json.as_ptr(),
                table.constraints_json.as_ptr(),
                table.fts_fields.as_ptr(),
                table.vector_fields.as_ptr()
            ),
            pointers
        );
        assert_eq!(table.security.role_owner, RoleIdentity::BOOTSTRAP);
        assert_eq!(table.columns[0].name, "id");
        assert_eq!(table.columns[0].ty, uqa_sql::ColumnType::Integer);
        assert_eq!(&*table.analyzer_json, "uninterpreted analyzer");
        assert_eq!(&*table.constraints_json, "uninterpreted constraints");
    }
    let expected = completed.capacity() * size_of::<PreparedTableDeclaration>()
        + completed
            .iter()
            .map(|table| {
                table.relation.reserved_bytes()
                    + table.security.reserved_bytes()
                    + table.analyzer_json.reserved_bytes()
                    + table.fts_fields.reserved_bytes()
                    + table.vector_fields.reserved_bytes()
                    + table.columns.retained_bytes()
                    + table.constraints_json.reserved_bytes()
            })
            .sum::<usize>();
    assert_eq!(
        control.memory().used(),
        expected,
        "only completed native owners remain"
    );
    drop(completed);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn prepared_table_authority_preserves_later_errors_before_any_hydration() {
    for defect in ["legacy", "owner", "column"] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut schemas = [schema("a"), schema("z")];
        if defect == "legacy" {
            schemas[1].security = RelationSecurityRow::legacy("uqa");
        } else {
            let RelationSecurityRow::Bound(row) = &mut schemas[1].security else {
                unreachable!()
            };
            if defect == "owner" {
                row.role_owner.object_id = [9; 16];
            } else {
                row.column_acls.insert("missing".into(), Vec::new());
            }
        }
        let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
        catalog
            .save_schema_row(&uqa_storage::SchemaRow::bootstrap("public"))
            .unwrap();
        for schema in &schemas {
            catalog.save_table(schema).unwrap();
        }
        let expected =
            super::super::restore_tables(&catalog, &RoleDefinitions::initial_catalog(), false)
                .unwrap_err()
                .to_string();
        let error = finish(&mut input(&schemas, &control)).err().unwrap();
        assert_eq!(error.to_string(), expected, "{defect}");
        assert_eq!(control.memory().used(), 0, "{defect}");
    }
}

#[test]
fn prepared_tables_retire_unread_and_completed_owners_at_every_pending_boundary() {
    let schemas = [schema("a"), schema("z")];
    let control = StorageReadControl::with_limit(1 << 20);
    let mut baseline = input(&schemas, &control);
    let mut pending = 0;
    while baseline.advance(1).unwrap().is_none() {
        pending += 1;
        assert!(pending < 16384);
    }
    drop(baseline);
    assert!(pending > 10);
    assert_eq!(control.memory().used(), 0);
    for stop in 0..=pending {
        for cancel in [false, true] {
            let control = StorageReadControl::with_limit(1 << 20);
            let mut input = input(&schemas, &control);
            for _ in 0..stop {
                assert!(input.advance(1).unwrap().is_none());
            }
            if cancel {
                control.cancellation().cancel();
                assert!(input.advance(1).is_err());
            }
            drop(input);
            assert_eq!(control.memory().used(), 0, "stop {stop}, cancel {cancel}");
        }
    }
}

#[test]
fn prepared_table_input_rejects_unrelated_or_exhausted_allowances() {
    let schemas = [schema("a")];
    let control = StorageReadControl::with_limit(1 << 20);
    let other = StorageReadControl::with_limit(1 << 20);
    assert!(TableDeclarationInput::new(
        tables(&schemas, &control),
        RoleDefinitions::initial_catalog(),
        &other
    )
    .is_err());
    assert_eq!((control.memory().used(), other.memory().used()), (0, 0));
    let tables = tables(&schemas, &control);
    let held = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    assert!(
        TableDeclarationInput::new(tables, RoleDefinitions::initial_catalog(), &control).is_err()
    );
    assert_eq!(control.memory().used(), held.bytes());
    drop(held);
    assert_eq!(control.memory().used(), 0);
    let mut input = input(&schemas, &control);
    let held = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    let error = finish(&mut input).err().unwrap();
    assert_eq!(
        crate::storage_errors::storage_error("prepare table", &error).sqlstate(),
        Some("53200")
    );
    drop(input);
    assert_eq!(control.memory().used(), held.bytes());
    drop(held);
    assert_eq!(control.memory().used(), 0);
}
