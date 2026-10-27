//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::{
    composites::{CompositeAttribute, CompositeTypeDescriptor},
    enums::EnumTypeLabels,
};
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Catalog(Arc<CompositeTypeDescriptor>);
impl SQLValueCatalog for Catalog {
    fn value_composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok((oid == self.0.type_oid).then(|| Arc::clone(&self.0)))
    }
    fn value_type_by_oid(&self, oid: u32) -> Result<Option<ColumnType>, SQLError> {
        Ok((oid == self.0.type_oid).then(|| {
            ColumnType::Composite(crate::ast::CompositeTypeReference {
                schema: "public".into(),
                name: "pair".into(),
                oid,
                relation_oid: 42_002,
                array_oid: 42_003,
            })
        }))
    }
    fn enum_type_labels(&self, _: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok(None)
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        Ok(None)
    }
    fn has_enum_types(&self) -> bool {
        false
    }
}

fn catalog() -> Catalog {
    Catalog(Arc::new(CompositeTypeDescriptor {
        type_oid: 42_001,
        relation_oid: 42_002,
        dropped: Vec::new(),
        attributes: ["a", "b"]
            .into_iter()
            .enumerate()
            .map(|(position, name)| CompositeAttribute {
                name: name.into(),
                ty: ColumnType::BigInteger,
                collation_oid: None,
                number: i16::try_from(position + 1).unwrap(),
            })
            .collect(),
    }))
}

fn binding(number: i16) -> AssignmentBinding {
    AssignmentBinding {
        path: vec![
            AssignmentPathStep::Subscripts {
                first_argument: 1,
                dimensions: 1,
                slice: false,
                bounds: ArraySliceBounds::default(),
            },
            AssignmentPathStep::Field {
                type_oid: 42_001,
                number,
            },
        ],
        value_argument: 2,
    }
}

#[test]
fn nested_array_fields_evaluate_each_operand_once_and_preserve_siblings() {
    let budget = MemoryBudget::new(65_536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let catalog = catalog();
    let mut current = control.copy_value(&Value::Null).unwrap();
    for (field, value) in [(1, 7), (2, 11)] {
        let inputs = [&*current, &Value::Int(2), &Value::Int(value)];
        let mut visits = [0; 3];
        current = evaluate(&binding(field), 3, Some(&catalog), &control, |position| {
            visits[position] += 1;
            Ok(inputs[position])
        })
        .unwrap();
        assert_eq!(visits, [1, 1, 1]);
    }
    let Value::Array(array) = &*current else {
        panic!("array assignment output");
    };
    assert!(array.element_at(&[1]).is_none());
    let value = array.element_at(&[2]).unwrap();
    let decoded = match value {
        Value::Datum(datum) => Some(
            crate::expr::datums::read_with_value_catalog_and_control(
                datum,
                Some(&catalog),
                &control,
            )
            .unwrap(),
        ),
        _ => None,
    };
    let Value::Record(record) = decoded.as_deref().unwrap_or(value) else {
        panic!("typed fields");
    };
    assert_eq!(record.len(), 2);
    for ((name, value), (expected_name, expected_value)) in record.iter().zip([("a", 7), ("b", 11)])
    {
        assert_eq!(name, expected_name);
        let decoded = match value {
            Value::Datum(datum) => Some(
                crate::expr::datums::read_with_value_catalog_and_control(
                    datum,
                    Some(&catalog),
                    &control,
                )
                .unwrap(),
            ),
            _ => None,
        };
        assert_eq!(
            decoded.as_deref().unwrap_or(value),
            &Value::Int(expected_value)
        );
    }
    drop(decoded);
    drop(current);
    assert_eq!(budget.used(), 0);
}

#[test]
fn assignment_scratch_and_parent_values_release_on_budget_failure() {
    let catalog = catalog();
    let token = CancellationToken::new();
    let inputs = [Value::Null, Value::Int(2), Value::Int(7)];
    let mut failures = 0;
    let mut successes = 0;
    for limit in [0, 64, 256, 1024, 4096, 65_536] {
        let budget = MemoryBudget::new(limit);
        let control = ProductionControl::new(&budget, &token, &token);
        let result = evaluate(&binding(1), 3, Some(&catalog), &control, |position| {
            Ok(&inputs[position])
        });
        match &result {
            Ok(_) => successes += 1,
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"));
                failures += 1;
            }
        }
        drop(result);
        assert_eq!(budget.used(), 0, "budget {limit}");
    }
    assert!(failures > 0 && successes > 0);
}

#[test]
fn cancellation_precedes_assignment_operands() {
    let token = CancellationToken::new();
    token.cancel();
    let budget = MemoryBudget::new(65_536);
    let control = ProductionControl::new(&budget, &token, &token);
    let error = evaluate::<&Value>(&binding(1), 3, None, &control, |_| {
        panic!("cancelled operand reached")
    })
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(budget.used(), 0);
}
