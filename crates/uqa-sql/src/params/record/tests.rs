//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{CompositeTypeReference, FunctionBinding},
    expr::composites::{CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor},
};
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

struct Catalog;

fn ty() -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "public".into(),
        name: "pair".into(),
        oid: 20_001,
        array_oid: 20_002,
        relation_oid: 20_003,
    })
}

impl FunctionTypeResolver for Catalog {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        unreachable!()
    }
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(matches!(name, "pair" | "composite#20001").then(ty))
    }
}

impl CompositeTypeCatalog for Catalog {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        assert_eq!(oid, 20_001);
        Ok(Some(Arc::new(CompositeTypeDescriptor {
            dropped: vec![
                crate::expr::composites::DroppedCompositeAttribute::from_type(
                    2,
                    &ColumnType::Integer,
                ),
            ],
            type_oid: oid,
            relation_oid: 20_003,
            attributes: [
                ("number", ColumnType::SmallInteger, 1),
                ("text", ColumnType::Varchar(Some(2)), 3),
                (
                    "amount",
                    ColumnType::Numeric {
                        precision: Some(5),
                        scale: Some(2),
                    },
                    4,
                ),
            ]
            .into_iter()
            .map(|(name, ty, number)| CompositeAttribute {
                name: name.into(),
                ty,
                collation_oid: None,
                number,
            })
            .collect(),
        })))
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
    fn resolve_type_oid(&self, oid: u32) -> Result<Option<ColumnType>, String> {
        Ok((oid == 20_001).then(ty))
    }
}

fn input(number: i64) -> [SQLParam; 3] {
    [
        SQLParam::typed_scalar(Value::Int(number), ColumnType::Integer),
        SQLParam::typed_scalar(Value::Str("abcd".into()), ColumnType::Text),
        SQLParam::typed_scalar(Value::Str("12.3456".into()), ColumnType::Text),
    ]
}

#[test]
fn live_positions_and_explicit_casts_keep_the_original_allowance() {
    let budget = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let value =
        create_record_parameter_with_control("pair", &input(32767), &Catalog, &Catalog, &control)
            .unwrap();
    assert_eq!(value.declared_scalar_type(), Some(&ty()));
    let Value::Datum(datum) = value.scalar_value().unwrap() else {
        panic!("retained tuple")
    };
    let decoded =
        crate::expr::datums::read_with_catalog_and_control(datum, Some(&Catalog), &control)
            .unwrap();
    let Value::Record(record) = &*decoded else {
        panic!("record")
    };
    assert_eq!(record.type_oid(), Some(20_001));
    assert_eq!(
        record.descriptor().field_names(),
        ["number", "text", "amount"]
    );
    assert_eq!(
        crate::result::format_postgres_text(value.scalar_value().unwrap(), &ty(), Some(&Catalog))
            .unwrap(),
        "(32767,ab,12.35)"
    );
    assert!(budget.used() > 0);
    drop(decoded);
    drop(value);
    assert_eq!(budget.used(), 0);
}

#[test]
fn invalid_shapes_overflow_and_both_cancellation_sources_release_partial_input() {
    for allowance in [0, 64, 256, 1024, 16384] {
        let budget = MemoryBudget::new(allowance);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = create_record_parameter_with_control(
            "pair",
            &input(32768),
            &Catalog,
            &Catalog,
            &control,
        )
        .unwrap_err();
        assert!(matches!(error.sqlstate(), Some("22003" | "53200")));
        assert_eq!(budget.used(), 0);
        for fields in [0, 2] {
            let error = create_record_parameter_with_control(
                "pair",
                &input(1)[..fields],
                &Catalog,
                &Catalog,
                &control,
            )
            .unwrap_err();
            assert!(matches!(error.sqlstate(), Some("42846" | "53200")));
            assert_eq!(budget.used(), 0);
        }
        for token in [&original, &invoking] {
            token.cancel();
            let error = create_record_parameter_with_control(
                "pair",
                &input(1),
                &Catalog,
                &Catalog,
                &control,
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(budget.used(), 0);
            token.reset();
        }
    }
}

#[test]
fn anonymous_records_keep_null_and_modifier_metadata_without_input_conversion() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let fields = [
        SQLParam::typed_scalar(Value::Null, ColumnType::Varchar(Some(4))),
        SQLParam::typed_scalar(Value::Int(7), ColumnType::SmallInteger),
        SQLParam::Scalar(Value::Null),
    ];
    let value =
        create_record_parameter_with_control("record", &fields, &Catalog, &Catalog, &control)
            .unwrap();
    assert_eq!(value.declared_scalar_type(), Some(&ColumnType::Record));
    let Value::Row(row) = value.scalar_value().unwrap() else {
        panic!("row")
    };
    let metadata = row.field_types().unwrap();
    assert_eq!((metadata[0].oid, metadata[0].type_modifier), (1043, 8));
    assert_eq!(metadata[0].collation_oid, Some(100));
    assert_eq!(metadata[1].oid, 21);
    assert_eq!(metadata[2].oid, 705);
    assert_eq!(&**row, &[Value::Null, Value::Int(7), Value::Null]);
    drop(value);
    assert_eq!(budget.used(), 0);
}
