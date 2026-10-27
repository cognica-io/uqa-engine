//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::selected_source_columns;
use uqa_sql::{plan::source_projection::SourceProjection, ColumnIdentity, ColumnType, RowSchema};

#[test]
fn complete_rows_retain_requested_hidden_attributes_and_declaration_order() {
    let columns = vec!["z".into(), "a".into()];
    let schema = RowSchema::with_typed_virtual_identities(
        &RowSchema::new(columns.clone()),
        &[
            (
                ColumnIdentity::unqualified("tableoid"),
                Some(ColumnType::Oid),
            ),
            (ColumnIdentity::unqualified("xmin"), Some(ColumnType::Xid)),
        ],
    );
    let mut projection = SourceProjection::retaining_all();
    projection.extend(["tableoid", "xmin", "a", "output_alias"].map(String::from));
    assert_eq!(
        selected_source_columns(columns, Some(&projection), &schema, false),
        ["z", "a", "tableoid", "xmin"]
    );
}

#[test]
fn pruned_and_document_sources_keep_only_their_bound_input_demands() {
    let columns = vec!["z".into(), "a".into()];
    let schema = RowSchema::new(columns.clone());
    let mut projection = SourceProjection::default();
    projection.extend(["a", "output_alias"].map(String::from));
    assert_eq!(
        selected_source_columns(columns, Some(&projection), &schema, false),
        ["a"]
    );
    assert_eq!(
        selected_source_columns(Vec::new(), Some(&projection), &RowSchema::default(), true),
        ["a", "output_alias"]
    );
}
