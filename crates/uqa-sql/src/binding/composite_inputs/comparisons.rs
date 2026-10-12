//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DISTINCT predicates retain selected field operators and the declared input of an outer NULL test.

use crate::{
    ast::{BinaryOp, ColumnType, FunctionDispatch},
    FunctionTypeResolver, RowSchema, SQLError, SQLParam, ScalarExpr,
};

pub(super) fn retain_comparison_inputs(
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
    if matches!(
        binding.dispatch,
        Some(FunctionDispatch::ValueNullTest { .. })
    ) {
        if let [argument] = args.as_slice() {
            if let Some(ty) = crate::scalar_type_with_resolver(argument, schema, params, types)? {
                binding.argument_types =
                    vec![ty.user_type_identity().unwrap_or_else(|| ty.catalog_name())];
            }
        }
        return Ok(());
    }
    if binding.dispatch != Some(FunctionDispatch::IsDistinct) {
        return Ok(());
    }
    let [left, right] = args.as_slice() else {
        return Ok(());
    };
    let (Some(left), Some(right)) = (fields(left, types)?, fields(right, types)?) else {
        return Ok(());
    };
    crate::type_resolution::row_comparison::validate_width(left.len(), right.len(), true)?;
    for (left, right) in left.iter().zip(right) {
        let left_type = crate::scalar_type_with_resolver(left, schema, params, types)?;
        let right_type = crate::scalar_type_with_resolver(right, schema, params, types)?;
        crate::type_resolution::binary_operator_types(
            BinaryOp::Equal,
            left_type.as_ref(),
            right_type.as_ref(),
        )?;
    }
    *expression = crate::bind_type_introspection_with_resolver(
        crate::type_resolution::row_distinct::scalar_fields(left.to_vec(), right.to_vec()),
        schema,
        params,
        types,
    );
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
