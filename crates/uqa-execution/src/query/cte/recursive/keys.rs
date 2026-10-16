//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SEARCH and CYCLE records retain the selected fields of the recursive row.

use crate::OwnedPhysicalRow;
use uqa_core::{RecordFieldType, RowValue, Value};
use uqa_sql::{catalog::type_metadata, ColumnType, SQLError};

pub(super) fn record(
    row: &OwnedPhysicalRow,
    positions: &[usize],
    depth: Option<i64>,
) -> Result<Value, SQLError> {
    let capacity = positions.len() + usize::from(depth.is_some());
    let mut values = Vec::with_capacity(capacity);
    let mut fields = Vec::with_capacity(capacity);
    if let Some(depth) = depth {
        values.push(Value::Int(depth));
        fields.push(field_type(&ColumnType::BigInteger));
    }
    let view = row.view();
    for position in positions {
        values.push(view.value_at(*position).cloned().unwrap_or(Value::Null));
        if let Some(ty) = row.schema.column_type(*position) {
            fields.push(field_type(ty));
        }
    }
    let value = if fields.len() == values.len() {
        RowValue::typed(values, fields)?
    } else {
        RowValue::new(values)
    };
    Ok(Value::Row(value))
}

fn field_type(ty: &ColumnType) -> RecordFieldType {
    RecordFieldType {
        oid: type_metadata::pg_type_oid(ty) as u32,
        type_modifier: type_metadata::pg_type_modifier(ty) as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PhysicalRow, RowSchema};
    use uqa_core::memory::ProductionControl;

    #[test]
    fn recursive_key_records_keep_range_identity_and_depth() {
        for kind in ["numrange", "nummultirange"] {
            let ty = ColumnType::from_sql_name(kind).unwrap();
            let schema = RowSchema::with_types(vec!["r".into()], vec![Some(ty.clone())]);
            let rows = ["[1.0,3.00)", "[1.00,3.0)"].map(|text| {
                let text = if kind == "nummultirange" {
                    format!("{{{text}}}")
                } else {
                    text.into()
                };
                OwnedPhysicalRow::new(
                    schema.clone(),
                    PhysicalRow::from_values(vec![uqa_sql::expr::cast_value(
                        &Value::Str(text),
                        kind,
                    )
                    .unwrap()]),
                )
            });
            for depth in [None, Some(0), Some(2)] {
                let left = record(&rows[0], &[0], depth).unwrap();
                let right = record(&rows[1], &[0], depth).unwrap();
                assert!(uqa_sql::expr::equal_typed_values_with_enum_catalog(
                    &left,
                    &right,
                    &ProductionControl::uncontrolled(),
                    None,
                )
                .unwrap());
                let Value::Row(value) = &left else {
                    panic!("expected record")
                };
                let fields = value.field_types().unwrap();
                assert_eq!(fields.last(), Some(&field_type(&ty)));
                if depth.is_some() {
                    assert_eq!(fields.first(), Some(&field_type(&ColumnType::BigInteger)));
                }
                if let Some(depth) = depth {
                    let next = record(&rows[1], &[0], Some(depth + 1)).unwrap();
                    assert!(uqa_sql::expr::compare_typed_values_with_enum_catalog(
                        &left,
                        &next,
                        &ProductionControl::uncontrolled(),
                        None,
                        None,
                    )
                    .unwrap()
                    .is_lt());
                }
            }
        }
    }
}
