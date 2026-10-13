//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ColumnType;
use uqa_core::Value;

#[test]
fn declared_columns_preserve_layout_without_publishing_expression_metadata() {
    let mut columns = vec![
        ColumnDef::nullable("retained", ColumnType::Integer),
        ColumnDef::nullable("new", ColumnType::Text),
    ];
    columns[0].object_id = Some([7; 16]);
    columns[0].default = Some(Expr::Literal(Value::Int(5)));
    columns[0].default_catalog_oid = Some(40_000);
    columns[0].check = Some(Expr::Literal(Value::Bool(true)));
    columns[0].not_null = true;
    columns[1].collation_oid = Some(950);
    let mut allocations = 0;
    let declared = declared_row_columns(&mut columns, &mut |kind: &str| {
        assert_eq!(kind, "column");
        allocations += 1;
        Ok([8; 16])
    })
    .unwrap();
    assert_eq!(allocations, 1);
    assert_eq!(columns[0].object_id, Some([7; 16]));
    assert_eq!(columns[1].object_id, Some([8; 16]));
    assert_eq!(columns[0].attribute_number, Some(1));
    assert_eq!(columns[1].attribute_number, Some(2));
    assert_eq!(declared[0].object_id, columns[0].object_id);
    assert_eq!(declared[1].object_id, columns[1].object_id);
    assert_eq!(declared[1].collation_oid, Some(950));
    assert_eq!(declared[0].name, "retained");
    assert_eq!(declared[1].ty, ColumnType::Text);
    assert!(declared.iter().all(|column| {
        column.default.is_none()
            && column.default_catalog_oid.is_none()
            && column.check.is_none()
            && !column.not_null
    }));
    assert!(columns[0].default.is_some());
    assert_eq!(columns[0].default_catalog_oid, Some(40_000));
    assert!(columns[0].check.is_some());
    assert!(columns[0].not_null);
    let again = declared_row_columns(&mut columns, &mut |_: &str| {
        panic!("a retained column must not acquire another identity")
    })
    .unwrap();
    assert_eq!(
        serde_json::to_value(again).unwrap(),
        serde_json::to_value(declared).unwrap()
    );
}
