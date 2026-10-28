//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ir::{ConstantValueSource, RetainedConstantValue, RetainedScalarLiteral};
use std::sync::Arc;

#[derive(Debug)]
struct Resident {
    value: Value,
    visible: bool,
}

impl ConstantValueSource for Resident {
    fn read(&self, _: u64) -> Result<Value, crate::SQLError> {
        panic!("membership inspection must not read a retained value");
    }

    fn resident_value(&self, index: u64) -> Option<&Value> {
        (self.visible && index == 0).then_some(&self.value)
    }
}

fn analyzed(negated: bool) -> ScalarExpr {
    crate::bind_type_introspection(
        ScalarExpr::InList {
            expr: Box::new(ScalarExpr::Column("value".into())),
            list: vec![
                ScalarExpr::Literal(Value::Str("x".into())),
                ScalarExpr::Literal(Value::Str("y".into())),
            ],
            negated,
        },
        &crate::RowSchema::with_types(vec!["value".into()], vec![Some(crate::ColumnType::Text)]),
        &[],
    )
}

#[test]
fn retained_membership_borrows_resident_arrays_without_reading_or_copying() {
    for negated in [false, true] {
        for (empty, visible, nested) in [
            (false, true, false),
            (true, true, false),
            (false, false, false),
            (false, true, true),
        ] {
            let elements = if empty {
                Vec::new()
            } else {
                vec![Value::Str("x".repeat(4 * 1024 * 1024)), Value::Null]
            };
            let elements = if nested {
                vec![Value::List(elements)]
            } else {
                elements
            };
            let array = uqa_core::ArrayValue::try_new(elements).unwrap();
            let pointer = array.elements().as_ptr();
            let source = Arc::new(Resident {
                value: Value::Array(array),
                visible,
            });
            let mut expression = analyzed(negated);
            let ScalarExpr::Func { args, .. } = &mut expression else {
                panic!("analyzed membership");
            };
            args[1] = ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
                RetainedConstantValue::new(source, 0),
                None,
                None,
                None,
            ));
            let allocation = allocation_counter::measure(|| {
                let operands = membership_operands(&expression);
                if !visible || nested || (empty && negated) {
                    assert!(operands.is_none());
                } else {
                    let operands = operands.unwrap();
                    assert_eq!(operands.negated, negated);
                    let MembershipItems::Constants(values) = operands.items else {
                        panic!("resident constants");
                    };
                    assert_eq!(values.as_ptr(), pointer);
                    assert_eq!(values.len(), if empty { 0 } else { 2 });
                }
            });
            assert_eq!(allocation.bytes_total, 0);
            assert_eq!(allocation.bytes_current, 0);
        }
    }
}
