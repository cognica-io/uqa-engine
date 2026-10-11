//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite assignment retains admitted field identities instead of inferring them from carriers.

use super::*;
use crate::ast::CompositeTypeReference;
use uqa_core::{RecordFieldType, RowValue};

struct Catalog;

fn target() -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "public".into(),
        name: "assigned".into(),
        oid: 42_001,
        relation_oid: 42_002,
        array_oid: 42_003,
    })
}

impl CompositeTypeCatalog for Catalog {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>> {
        assert_eq!(oid, 42_001);
        Ok(Some(Arc::new(CompositeTypeDescriptor {
            type_oid: oid,
            relation_oid: 42_002,
            dropped: Vec::new(),
            attributes: vec![CompositeAttribute {
                name: "n".into(),
                ty: ColumnType::Xid,
                number: 1,
            }],
        })))
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }
    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
    fn resolve_type_name(&self, name: &str) -> std::result::Result<Option<ColumnType>, String> {
        Ok(ColumnType::from_sql_name(name).ok())
    }
}

fn row(value: Value, oid: u32) -> Value {
    Value::Row(
        RowValue::typed(
            vec![value],
            vec![RecordFieldType {
                oid,
                type_modifier: -1,
            }],
        )
        .unwrap(),
    )
}

#[test]
fn admitted_xid_fields_are_not_reinterpreted_as_integer_inputs() {
    for value in [0, 1, i64::from(u32::MAX)] {
        let assigned = cast_to_composite(
            &Catalog,
            &row(Value::Int(value), 28),
            Some(&ColumnType::Record),
            &target(),
        )
        .unwrap()
        .unwrap();
        let Value::Record(fields) = assigned else {
            panic!("assigned composite")
        };
        assert_eq!(fields.type_oid(), Some(42_001));
        assert_eq!(fields[0].1, Value::Int(value));
    }
}

#[test]
fn unknown_field_input_is_converted_while_a_declared_integer_remains_incompatible() {
    let assigned = cast_to_composite(
        &Catalog,
        &row(Value::Str("42".into()), 705),
        Some(&ColumnType::Record),
        &target(),
    )
    .unwrap()
    .unwrap();
    let Value::Record(fields) = assigned else {
        panic!("assigned composite")
    };
    assert_eq!(fields[0].1, Value::Int(42));
    let error = cast_to_composite(
        &Catalog,
        &row(Value::Int(42), 23),
        Some(&ColumnType::Record),
        &target(),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42846"));
}
