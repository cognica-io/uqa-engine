//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite assignment retains admitted field identities instead of inferring them from carriers.

use super::*;
use crate::ast::CompositeTypeReference;
use uqa_core::{RecordFieldType, RowValue};

struct Catalog(ColumnType);

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
                collation_oid: None,
                name: "n".into(),
                ty: self.0.clone(),
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
                collation_oid: None,
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
            &Catalog(ColumnType::Xid),
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
fn admitted_bpchar_fields_keep_padding_in_composite_assignment() {
    let catalog = Catalog(ColumnType::Bpchar);
    let field = Value::FixedChar("a   ".into());
    let assigned = cast_to_composite(
        &catalog,
        &row(field.clone(), 1042),
        Some(&ColumnType::Record),
        &target(),
    )
    .unwrap()
    .unwrap();
    let Value::Record(fields) = &assigned else {
        panic!("assigned composite")
    };
    assert_eq!(fields[0].1, field);
    assert_eq!(
        crate::result::format_postgres_text(&assigned, &target(), Some(&catalog)).unwrap(),
        r#"("a   ")"#
    );
}

#[test]
fn unknown_field_input_is_converted_while_a_declared_integer_remains_incompatible() {
    let assigned = cast_to_composite(
        &Catalog(ColumnType::Xid),
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
        &Catalog(ColumnType::Xid),
        &row(Value::Int(42), 23),
        Some(&ColumnType::Record),
        &target(),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42846"));
}

#[test]
fn builtin_array_field_identities_resolve_without_a_user_catalog_oid_lookup() {
    for (oid, element_type, element) in [
        (1007, ColumnType::Integer, Value::Int(7)),
        (1009, ColumnType::Text, Value::Str("7".into())),
        (1022, ColumnType::DoublePrecision, Value::Float(7.5)),
        (1011, ColumnType::Xid, Value::Int(i64::from(u32::MAX))),
    ] {
        let input = Value::Array(uqa_core::ArrayValue::try_new(vec![element.clone()]).unwrap());
        let assigned = cast_to_composite(
            &Catalog(ColumnType::Array(Box::new(element_type))),
            &row(input, oid),
            Some(&ColumnType::Record),
            &target(),
        )
        .unwrap()
        .unwrap();
        let Value::Record(fields) = assigned else {
            panic!("assigned composite")
        };
        let Value::Array(array) = &fields[0].1 else {
            panic!("assigned array")
        };
        assert_eq!(array.elements(), &[element]);
    }
}
