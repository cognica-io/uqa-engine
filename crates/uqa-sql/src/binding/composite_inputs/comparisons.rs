//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ROW DISTINCT predicates select independent field operators before evaluation.

use crate::{
    ast::{BinaryOp, ColumnType, FunctionBinding, FunctionDispatch, FunctionOrderSyntax},
    FunctionTypeResolver, RowSchema, SQLError, SQLParam, ScalarExpr,
};

pub(super) fn retain_row_distinct(
    expression: &mut ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    types: &dyn FunctionTypeResolver,
) -> Result<(), SQLError> {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = expression
    else {
        return Ok(());
    };
    if binding.dispatch != Some(FunctionDispatch::IsDistinct) {
        return Ok(());
    }
    let [left, right] = args.as_slice() else {
        return Ok(());
    };
    let (Some(left), Some(right)) = (fields(left, types)?, fields(right, types)?) else {
        return Ok(());
    };
    if left.len() != right.len() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "unequal number of entries in row expressions".into(),
        });
    }
    let mut comparisons = Vec::with_capacity(left.len());
    for (left, right) in left.iter().zip(right) {
        let left_type = crate::scalar_type_with_resolver(left, schema, params, types)?;
        let right_type = crate::scalar_type_with_resolver(right, schema, params, types)?;
        crate::type_resolution::binary_operator_types(
            BinaryOp::Equal,
            left_type.as_ref(),
            right_type.as_ref(),
        )?;
        let binding = FunctionBinding::dispatched(FunctionDispatch::IsDistinct);
        let comparison = ScalarExpr::Func {
            name: binding.name.clone(),
            binding: Some(binding),
            args: vec![value_operand(left), value_operand(right)],
            distinct: false,
            order_syntax: FunctionOrderSyntax::Ordinary,
            order_by: Vec::new(),
            filter: None,
        };
        comparisons.push(crate::bind_type_introspection_with_resolver(
            comparison, schema, params, types,
        ));
    }
    *expression = if comparisons.is_empty() {
        ScalarExpr::Literal(uqa_core::Value::Bool(false))
    } else if comparisons.len() == 1 {
        comparisons.pop().expect("one field comparison")
    } else {
        ScalarExpr::Or(comparisons)
    };
    Ok(())
}

fn fields<'a>(
    expression: &'a ScalarExpr,
    types: &dyn FunctionTypeResolver,
) -> Result<Option<&'a [ScalarExpr]>, SQLError> {
    Ok(match expression {
        ScalarExpr::Row(items) | ScalarExpr::CompositeRow { items, .. } => Some(items),
        ScalarExpr::Cast {
            expr,
            ty,
            implicit: false,
        } if types
            .resolve_type_name(ty)?
            .is_some_and(|ty| matches!(ty, ColumnType::Record)) =>
        {
            match expr.as_ref() {
                ScalarExpr::Row(items) | ScalarExpr::CompositeRow { items, .. } => Some(items),
                _ => None,
            }
        }
        _ => None,
    })
}

fn value_operand(expression: &ScalarExpr) -> ScalarExpr {
    // A nested ROW is one record-valued field. The implicit record coercion retains that selection when this already-analyzed predicate is bound again.
    if matches!(
        expression,
        ScalarExpr::Row(_) | ScalarExpr::CompositeRow { .. }
    ) {
        ScalarExpr::Cast {
            implicit: true,
            expr: Box::new(expression.clone()),
            ty: ColumnType::Record.catalog_name(),
        }
    } else {
        expression.clone()
    }
}
