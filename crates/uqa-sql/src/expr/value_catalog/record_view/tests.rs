//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::IntervalFields,
    catalog::type_metadata::{pg_type_modifier, pg_type_oid},
    expr::{
        composites::{retain_value_with_control, CompositeAttribute, CompositeTypeCatalog},
        enums::EnumTypeLabels,
    },
    ColumnType,
};
use std::cell::RefCell;
use uqa_core::{memory::MemoryBudget, CancellationToken, RecordFieldType, RecordValue, RowValue};

struct Catalog(RefCell<Arc<CompositeTypeDescriptor>>);

impl CompositeTypeCatalog for Catalog {
    fn composite_type(&self, _: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok(Some(self.0.borrow().clone()))
    }
}
impl SQLValueCatalog for Catalog {
    fn value_composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        self.composite_type(oid)
    }
    fn enum_type_labels(&self, _: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        panic!("record observation does not invoke enum input")
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        panic!("record observation does not invoke enum input")
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        panic!("record observation does not invoke enum input")
    }
    fn has_enum_types(&self) -> bool {
        false
    }
}

#[test]
fn anonymous_field_types_preserve_modifiers_on_null_without_coercion() {
    let types = vec![
        ColumnType::Real,
        ColumnType::Varchar(Some(12)),
        ColumnType::Character(5),
        ColumnType::Numeric {
            precision: Some(7),
            scale: Some(-2),
        },
        ColumnType::Numeric {
            precision: Some(3),
            scale: Some(5),
        },
        ColumnType::Bit(Some(17)),
        ColumnType::VarBit(Some(31)),
        ColumnType::TimePrecision(3),
        ColumnType::TimeTzPrecision(2),
        ColumnType::TimestampPrecision(4),
        ColumnType::TimestampTzPrecision(1),
        ColumnType::IntervalWithFields {
            fields: IntervalFields::DayToSecond,
            precision: Some(3),
        },
        ColumnType::Array(Box::new(ColumnType::Numeric {
            precision: Some(11),
            scale: Some(-3),
        })),
    ];
    let row = Value::Row(
        RowValue::typed(
            vec![Value::Null; types.len()],
            types
                .iter()
                .map(|ty| RecordFieldType {
                    oid: pg_type_oid(ty) as u32,
                    type_modifier: pg_type_modifier(ty) as i32,
                    collation_oid: Some(0),
                })
                .collect(),
        )
        .unwrap(),
    );
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let view = SQLRecordView::new(&row, None, None, &control).unwrap();
    assert_eq!(view.len(), types.len());
    for (index, ty) in types.iter().enumerate() {
        let field = view
            .field_with_control(index, None, &control)
            .unwrap()
            .unwrap();
        assert_eq!(field.scalar_value(), Some(&Value::Null));
        assert_eq!(field.declared_scalar_type(), Some(ty));
    }
    assert!(view
        .field_with_control(types.len(), None, &control)
        .unwrap()
        .is_none());
    assert_eq!(budget.used(), 0);
    token.cancel();
    assert!(view.field_with_control(0, None, &control).is_err());
    assert_eq!(budget.used(), 0);
}

#[test]
fn physical_selection_pins_layout_and_keeps_later_invalid_fields_lazy() {
    let catalog = Catalog(RefCell::new(Arc::new(CompositeTypeDescriptor {
        type_oid: 20_001,
        relation_oid: 20_003,
        dropped: vec![],
        attributes: ["a", "b", "c"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| CompositeAttribute {
                name: name.into(),
                ty: ColumnType::Integer,
                number: index as i16 + 1,
                collation_oid: None,
            })
            .collect(),
    })));
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let record = Value::Record(RecordValue::from_parts(
        vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(2)),
            ("c".into(), Value::Int(3)),
        ],
        Some(20_001),
    ));
    let value = retain_value_with_control(
        control.copy_value(&record).unwrap(),
        Some(&catalog),
        &control,
    )
    .unwrap();
    let original = SQLRecordView::new(&value, None, Some(&catalog), &control).unwrap();
    Arc::make_mut(&mut catalog.0.borrow_mut()).attributes[0].ty = ColumnType::BigInteger;
    let current = SQLRecordView::new(&value, None, Some(&catalog), &control).unwrap();
    let old = original
        .field_with_control(0, Some(&catalog), &control)
        .unwrap()
        .unwrap();
    let new = current
        .field_with_control(0, Some(&catalog), &control)
        .unwrap()
        .unwrap();
    assert_eq!(old.declared_scalar_type(), Some(&ColumnType::Integer));
    assert_eq!(new.declared_scalar_type(), Some(&ColumnType::BigInteger));
    let read = |parameter: &SQLParam| {
        let Some(Value::Datum(datum)) = parameter.scalar_value() else {
            panic!("physical field");
        };
        crate::expr::datums::read_with_control(datum, &control)
    };
    assert_eq!(*read(&old).unwrap(), Value::Int(1));
    assert_eq!(*read(&new).unwrap(), Value::Int((2_i64 << 32) | 1));
    let tail = current
        .field_with_control(2, Some(&catalog), &control)
        .unwrap()
        .unwrap();
    assert_eq!(read(&tail).unwrap_err().sqlstate(), Some("XX001"));
    assert!(current
        .field_with_control(3, Some(&catalog), &control)
        .unwrap()
        .is_none());
    drop((old, new, tail, value));
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_descriptor_copy_releases_partial_names_and_types() {
    let descriptor = CompositeTypeDescriptor {
        type_oid: 20_001,
        relation_oid: 20_002,
        dropped: vec![],
        attributes: vec![CompositeAttribute {
            name: "field".repeat(100),
            ty: ColumnType::Array(Box::new(ColumnType::Text)),
            number: 1,
            collation_oid: None,
        }],
    };
    let token = CancellationToken::new();
    for limit in [0, 200, 600] {
        let budget = MemoryBudget::new(limit);
        let control = ProductionControl::new(&budget, &token, &token);
        assert!(descriptor.clone_with_control(&control).is_err());
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(65536);
    let control = ProductionControl::new(&budget, &token, &token);
    let copy = descriptor.clone_with_control(&control).unwrap();
    assert_eq!(*copy, descriptor);
    assert!(budget.used() > 500);
    drop(copy);
    assert_eq!(budget.used(), 0);
}
