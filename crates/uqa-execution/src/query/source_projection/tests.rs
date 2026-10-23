//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{physical::run_to_batches, PhysicalRow, RowSchema, TableScan};
use uqa_core::Value;

#[test]
fn source_qualification_and_join_aliases_retain_empty_namespaces() {
    let scan = || {
        Box::new(TableScan::from_physical_rows(
            RowSchema::default(),
            vec![PhysicalRow::default()],
        )) as Box<dyn PhysicalOperator>
    };
    let qualified = qualify_source_operator_with_columns(scan(), &[], "q", None, &[], false);
    assert!(qualified.row_schema().has_qualifier("q"));
    let alias = alias_join_operator(qualified, Some("j"), &[]).unwrap();
    assert!(alias.row_schema().has_qualifier("j"));
    assert!(!alias.row_schema().has_qualifier("q"));
    for mut operator in [
        qualify_source_operator_with_columns(scan(), &[], "q", None, &[], false),
        alias,
    ] {
        let schema = operator.row_schema().clone();
        let output = run_to_batches(operator.as_mut()).unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].schema, schema);
        assert_eq!(output[0].rows.len(), 1);
        assert_eq!(output[0].rows[0].value(0), None);
    }
}

#[test]
fn pruning_visible_recursive_columns_retains_hidden_control_fields() {
    let schema = RowSchema::with_types(
        vec!["value".into(), "path".into()],
        vec![Some(uqa_sql::ColumnType::Integer); 2],
    );
    let scan = Box::new(TableScan::from_physical_rows(
        schema,
        vec![PhysicalRow::from_values(vec![Value::Int(1), Value::Int(2)])],
    ));
    let hidden = crate::ColumnSelection::hiding_trailing_columns(scan, 1, "r");
    let mut requested = uqa_sql::plan::source_projection::SourceProjection::default();
    requested.insert("path".into());
    let prune = ColumnPrune::from([("r".into(), requested)]);
    let mut operator = qualify_source_operator_with_columns(
        Box::new(hidden),
        &["value".into()],
        "r",
        Some(&prune),
        &[],
        false,
    );
    assert!(operator.row_schema().is_empty());
    assert!(operator.row_schema().has_qualified_column("r", "path"));
    let slot = operator.row_schema().column_slot("path").unwrap();
    let output = run_to_batches(operator.as_mut()).unwrap();
    assert_eq!(output[0].rows[0].value(slot), Some(&Value::Int(2)));
}
