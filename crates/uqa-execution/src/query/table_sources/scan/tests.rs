//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical table source schemas preserve declaration metadata across pruning and aliasing.

use super::projected_scan_schema;
use crate::query::scored_input::{ScoredDocumentSource, ScoredInput};
use crate::query::table_read::TableRead;
use crate::RowSource;
use parking_lot::{RwLock, RwLockReadGuard};
use std::sync::Arc;
use uqa_sql::ast::{ColumnDef, ColumnType};
use uqa_sql::schema::ScalarTypeSchema;
use uqa_sql::Statement;
use uqa_storage::DocumentStore;

fn columns() -> Vec<ColumnDef> {
    let Statement::CreateTable(mut table) =
        uqa_sql::compile("CREATE TABLE t (unused integer, c text, p text, plain text)")
            .unwrap()
            .remove(0)
    else {
        panic!("table declaration")
    };
    table.columns[1].collation_oid = Some(950);
    table.columns[2].collation_oid = Some(951);
    table.columns
}

#[test]
fn pruned_scan_collations_follow_the_original_field_not_its_public_alias() {
    let definitions = columns();
    for include_oid in [false, true] {
        let physical = ["p", "c", "plain", "xmin"].map(String::from);
        // Swapping public labels must not swap the declarations used to read the fields.
        let mut visible = ["c", "p", "other", "xmin"].map(String::from).to_vec();
        if include_oid {
            visible.push("tableoid".into());
        }
        let schema =
            projected_scan_schema("q", &physical, &visible, &definitions, include_oid, None)
                .unwrap();
        assert_eq!(schema.columns(), visible);
        assert_eq!(schema.qualified_collation("q", "c"), Some(951));
        assert_eq!(schema.qualified_collation("q", "p"), Some(950));
        assert_eq!(schema.qualified_collation("q", "other"), Some(100));
        assert_eq!(schema.column_type(3), Some(&ColumnType::Xid));
        assert_eq!(schema.column_collation(3), Some(0));
        assert!(!schema.wildcard_position_visible(3));
        assert_eq!(schema.physical_width(), visible.len());
        if include_oid {
            assert_eq!(schema.column_type(4), Some(&ColumnType::Oid));
            assert_eq!(schema.column_collation(4), Some(0));
            assert!(!schema.wildcard_position_visible(4));
        }
    }
}

#[test]
fn scan_whole_row_identity_survives_aliases_and_empty_projection() {
    let row_type = uqa_sql::ast::CompositeTypeReference {
        schema: "public".into(),
        name: "stored_row".into(),
        oid: 50_101,
        array_oid: 50_102,
        relation_oid: 50_100,
    };
    for fields in [Vec::new(), vec!["c".into()]] {
        let visible = fields.iter().map(|_| "renamed".into()).collect::<Vec<_>>();
        let schema = projected_scan_schema(
            "alias",
            &fields,
            &visible,
            &columns(),
            false,
            Some(row_type.clone()),
        )
        .unwrap();
        assert_eq!(schema.columns(), visible);
        assert_eq!(
            schema.whole_row_type("alias"),
            Some(&ColumnType::Composite(row_type.clone()))
        );
        assert!(schema.whole_row_type("stored_row").is_none());
    }
}

struct DeclaredTable {
    columns: Vec<ColumnDef>,
}

impl TableRead for DeclaredTable {
    fn column_definitions(&self) -> Vec<ColumnDef> {
        self.columns.clone()
    }

    fn maps_integer_keys(&self) -> bool {
        false
    }

    fn read_documents(&self) -> RwLockReadGuard<'_, Box<dyn DocumentStore>> {
        panic!("constructing metadata must not read documents")
    }
}

#[test]
fn empty_scored_sources_retain_selected_collations_without_reading_values() {
    let source = ScoredDocumentSource::new(
        "t",
        Arc::new(DeclaredTable { columns: columns() }),
        ScoredInput::entries(Vec::new(), true),
        ["p", "c", "plain", "xmin", "tableoid"]
            .map(String::from)
            .to_vec(),
        None,
        None,
    );
    let schema = source.physical_schema().unwrap();
    assert_eq!(schema.collation_of("p"), Some(951));
    assert_eq!(schema.collation_of("c"), Some(950));
    assert_eq!(schema.collation_of("plain"), Some(100));
    assert_eq!(schema.column_collation(3), Some(0));
    assert_eq!(schema.column_collation(4), Some(0));
}

struct DocumentTable(RwLock<Box<dyn DocumentStore>>);

impl TableRead for DocumentTable {
    fn column_definitions(&self) -> Vec<ColumnDef> {
        Vec::new()
    }
    fn maps_integer_keys(&self) -> bool {
        false
    }
    fn read_documents(&self) -> RwLockReadGuard<'_, Box<dyn DocumentStore>> {
        self.0.read()
    }
}

#[test]
fn document_field_projection_does_not_copy_unrequested_payload() {
    use crate::query::local_table::{LocalTableRowSource, LocalTableScanConfig};
    use uqa_core::Value;
    use uqa_sql::plan::source_projection::SourceProjection;
    use uqa_storage::{MemoryDocumentStore, StoredDocument};

    let mut rows = MemoryDocumentStore::new();
    rows.put_stored(
        1,
        StoredDocument::new(
            [
                ("value".into(), Value::Int(7)),
                ("body".into(), Value::Str("x".repeat(2 * 1024 * 1024))),
            ]
            .into(),
        ),
    )
    .unwrap();
    let mut projection = SourceProjection::retaining_all();
    projection.extend(["value".into(), "absent".into()]);
    let columns = super::requested_document_fields(&projection)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let schema = projected_scan_schema("d", &columns, &columns, &[], false, None).unwrap();
    let mut scan = LocalTableRowSource::new(LocalTableScanConfig {
        cancellation: uqa_core::CancellationToken::new(),
        serializable: None,
        table_name: "docs".into(),
        table: Arc::new(DocumentTable(RwLock::new(Box::new(rows)))),
        column_definitions: Arc::new(Vec::new()),
        columns: columns.clone(),
        schema: columns,
        physical_schema: schema,
        metadata: projection.metadata(),
        table_oid: None,
        predicate: None,
        estimated_cardinality: 1,
        lock_origin: None,
        recheck_pins: None,
        candidates: None,
        command_changes: None,
    });
    let allocations = allocation_counter::measure(|| {
        let selected = scan.next_batch(1).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0]["value"], Value::Int(7));
        assert_eq!(selected[0]["absent"], Value::Null);
        assert_eq!(selected[0].len(), 2);
    });
    assert!(
        allocations.bytes_total < 64 * 1024,
        "a two-field projection copied the 2 MiB payload: {allocations:?}"
    );
    assert!(scan.next_batch(1).unwrap().is_empty());
}
