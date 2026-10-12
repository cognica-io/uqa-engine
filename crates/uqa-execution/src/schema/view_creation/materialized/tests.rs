//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::materialized_rows;
use uqa_core::Value;
use uqa_sql::SQLResult;

#[test]
fn materialized_rows_preserve_duplicate_query_values_by_position() {
    let result = SQLResult::from_rows_with_positions(
        vec!["value".into(), "value".into()],
        vec![[("value".into(), Value::Int(2))].into()],
        Some(vec![vec![Value::Int(1), Value::Int(2)]]),
    );
    let rows = materialized_rows(&result, &["left".into(), "right".into()]).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["left"], Value::Int(1));
    assert_eq!(rows[0]["right"], Value::Int(2));
}

#[test]
fn materialized_row_schema_drift_is_reported_even_without_rows() {
    let result = SQLResult::from_rows(vec!["only".into()], Vec::new());
    let error = materialized_rows(&result, &["first".into(), "second".into()]).unwrap_err();
    assert!(error.to_string().contains("schema width 2 changed to 1"));
    let malformed = SQLResult::from_rows(
        vec!["first".into(), "second".into()],
        vec![[("first".into(), Value::Int(1))].into()],
    );
    let error = materialized_rows(&malformed, &["first".into(), "second".into()]).unwrap_err();
    assert!(error.to_string().contains("row 0 is missing column 1"));
}

#[test]
fn materialized_refresh_keeps_created_types_collations_and_atomic_population_state() {
    use crate::catalog::view::{StoredView, StoredViewKind};
    use uqa_sql::{
        ast::RelationPersistence, catalog::stored_view::StoredViewDefinition, ColumnType,
    };
    let uqa_sql::plan::UnifiedPlan::Query(query) = uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile("SELECT NULL::text COLLATE \"C\" AS value")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let mut view = StoredView {
        security: uqa_sql::catalog::security::BoundTableSecurity::owner(
            uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
        ),
        definition: StoredViewDefinition {
            object_id: [1; 16],
            query: *query,
            output_columns: Some(vec!["renamed".into()]),
            persistence: RelationPersistence::Permanent,
            options: Vec::new(),
            kind: StoredViewKind::Materialized,
            materialized_rows: Vec::new(),
            materialized_column_types: vec![Some(ColumnType::Text)],
            materialized_column_collations: Some(vec![950]),
            populated: false,
            catalog_oids: None,
            row_type_array_name: None,
        },
    };
    let result = SQLResult::from_rows(
        vec!["value".into()],
        vec![[("value".into(), Value::Str("new".into()))].into()],
    );
    let before = serde_json::to_value(&view).unwrap();
    super::replace_materialized_rows(&mut view, Some(&result)).unwrap();
    assert!(view.populated);
    assert_eq!(
        view.materialized_rows[0]["renamed"],
        Value::Str("new".into())
    );
    assert_eq!(view.materialized_column_types, [Some(ColumnType::Text)]);
    assert_eq!(
        view.materialized_schema().unwrap().column_collation(0),
        Some(950)
    );
    let populated = serde_json::to_value(&view).unwrap();
    let invalid = SQLResult::from_rows(Vec::new(), Vec::new());
    assert!(super::replace_materialized_rows(&mut view, Some(&invalid)).is_err());
    assert_eq!(serde_json::to_value(&view).unwrap(), populated);
    super::replace_materialized_rows(&mut view, None).unwrap();
    assert_eq!(serde_json::to_value(&view).unwrap(), before);
}
