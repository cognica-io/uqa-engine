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
use parking_lot::RwLockReadGuard;
use std::sync::Arc;
use uqa_sql::ast::{ColumnDef, ColumnType};
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
        let schema = projected_scan_schema("q", &physical, &visible, &definitions, include_oid);
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
