//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{physical::run_to_batches, PhysicalRow, RowSchema, TableScan};

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
