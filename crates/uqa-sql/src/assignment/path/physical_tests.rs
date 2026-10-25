//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::assignment::containers::assign_array_with_control;
use crate::{
    ast::CompositeTypeReference,
    expr::{
        composites::{CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor},
        EngineHook,
    },
};
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Catalog;

fn composite_type() -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "public".into(),
        name: "container".into(),
        oid: 20_001,
        array_oid: 20_002,
        relation_oid: 20_003,
    })
}

impl CompositeTypeCatalog for Catalog {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok((oid == 20_001).then(|| {
            Arc::new(CompositeTypeDescriptor {
                type_oid: oid,
                relation_oid: 20_003,
                dropped: vec![],
                attributes: [
                    ("a", ColumnType::Integer),
                    ("items", ColumnType::Array(Box::new(ColumnType::Integer))),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (name, ty))| CompositeAttribute {
                    name: name.into(),
                    ty,
                    collation_oid: None,
                    number: index as i16 + 1,
                })
                .collect(),
            })
        }))
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("field writes must not repeat input effects")
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
        Ok((oid == 20_001).then(composite_type))
    }
}

fn physical_input() -> Value {
    crate::expr::composites::composite_from_text(&Catalog, "(7,\"{1,2}\")", 20_001).unwrap()
}

#[test]
fn nested_partial_write_reads_physical_containers_without_repeating_input() {
    let original = physical_input();
    let path = [
        AssignmentContainer::Field {
            type_oid: 20_001,
            number: 2,
        },
        AssignmentContainer::subscripts(&[(None, Some(2))], false).unwrap(),
    ];
    let result = assign_with_control(
        &original,
        &path,
        &Value::Int(9),
        Some(&crate::expr::EngineValueCatalog(&Catalog)),
        &ProductionControl::uncontrolled(),
    )
    .unwrap()
    .into_uncontrolled()
    .unwrap();
    let Value::Datum(record) = result else {
        panic!("retained rebuilt record");
    };
    let field = |name: &str| {
        let value = crate::expr::datums::read_composite_field_with_control(
            &record,
            name,
            Some(&Catalog),
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
        match &*value {
            Value::Datum(datum) => {
                crate::expr::datums::read_with_catalog(datum, Some(&Catalog)).unwrap()
            }
            value => value.clone(),
        }
    };
    let Value::Array(array) = field("items") else {
        panic!("rebuilt array");
    };
    assert_eq!(array.element_at(&[2]), Some(&Value::Int(9)));
    assert_eq!(field("a"), Value::Int(7));
    assert!(matches!(original, Value::Datum(_)));
}

#[test]
fn physical_array_assignment_keeps_read_and_copy_under_one_allowance() {
    let Value::Datum(record) = physical_input() else {
        panic!("physical tuple")
    };
    let array = crate::expr::datums::read_composite_field_with_control(
        &record,
        "items",
        Some(&Catalog),
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    for bytes in [0, 8192] {
        let budget = MemoryBudget::new(bytes);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let result = assign_array_with_control(
            &array,
            &Value::Null,
            &[(Some(1), Some(2))],
            true,
            Some(&crate::expr::EngineValueCatalog(&Catalog)),
            &control,
        );
        if bytes == 0 {
            assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        } else {
            let result = result.unwrap();
            assert_eq!(budget.used(), result.reserved_bytes());
            assert!(result.reserved_bytes() > 0);
            drop(result);
        }
        assert_eq!(budget.used(), 0);
    }
}
