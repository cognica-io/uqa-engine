//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native operator identity transfers with its stored coercions and survives a second binding.

use super::{expression, Catalog};
use crate::{ast::FunctionDispatch, ColumnType, RowSchema, ScalarExpr};

#[test]
fn stored_concatenation_identity_and_input_roles_bind_once() {
    let array = ColumnType::Array(Box::new(ColumnType::Integer));
    for (left, right, expected) in [
        (
            ColumnType::Text,
            ColumnType::Text,
            FunctionDispatch::Concat { oid: Some(654) },
        ),
        (
            ColumnType::Text,
            ColumnType::Integer,
            FunctionDispatch::Concat { oid: Some(2779) },
        ),
        (
            ColumnType::Integer,
            ColumnType::Text,
            FunctionDispatch::Concat { oid: Some(2780) },
        ),
        (
            array.clone(),
            ColumnType::Integer,
            FunctionDispatch::ArrayAppend,
        ),
        (
            ColumnType::Integer,
            array.clone(),
            FunctionDispatch::ArrayPrepend,
        ),
        (array.clone(), array, FunctionDispatch::ArrayConcat),
    ] {
        let schema =
            RowSchema::with_types(vec!["a".into(), "b".into()], vec![Some(left), Some(right)]);
        for legacy in [false, true] {
            let mut value = expression("a || b");
            if legacy {
                let ScalarExpr::Func { name, binding, .. } = &mut value else {
                    panic!("concatenation")
                };
                *name = "concat_op".into();
                *binding = None;
            }
            assert!(
                super::super::store_operand_coercions(&mut value, &schema, &[], &Catalog).unwrap()
            );
            let ScalarExpr::Func {
                binding: Some(binding),
                ..
            } = &value
            else {
                panic!("stored operator identity")
            };
            assert_eq!(binding.dispatch, Some(expected));
            let encoded = serde_json::to_string(&value).unwrap();
            let mut restored = serde_json::from_str(&encoded).unwrap();
            assert!(
                !super::super::store_operand_coercions(&mut restored, &schema, &[], &Catalog)
                    .unwrap()
            );
            assert_eq!(serde_json::to_string(&restored).unwrap(), encoded);
        }
    }
}
