//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recursive node inference keeps unrelated constructor storage off each child's stack.

use super::{
    cast_compatibility, common, copy_type, operators, resolved_type_name,
    scalar_type_inner_with_control, ColumnType, FunctionTypeResolver, Produced, ProductionControl,
    SQLError, SQLParam, ScalarExpr, ScalarTypeSchema,
};
use uqa_core::memory::ProductionVec;

pub(super) fn collated_type_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let ScalarExpr::Collated { expr, ty, .. } = expression else {
        unreachable!("collated expression");
    };
    let inner = scalar_type_inner_with_control(expr, schema, params, resolver, control)?;
    if let Some(inner) = inner {
        if *inner == *ty
            || *inner.without_type_modifiers_with_control(control)?
                == *ty.without_type_modifiers_with_control(control)?
        {
            return Ok(Some(inner));
        }
    }
    copy_type(Some(ty), control)
}

pub(super) fn cast_type_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let ScalarExpr::Cast { expr, ty, .. } = expression else {
        unreachable!("cast expression");
    };
    let source = scalar_type_inner_with_control(expr, schema, params, resolver, control)?;
    let target = resolved_type_name(ty, resolver, control)?;
    if let Some(target) = target.as_ref() {
        cast_compatibility::validate_explicit_cast_with_control(
            source.as_deref(),
            target,
            control,
        )?;
    }
    if source
        .as_deref()
        .zip(target.as_deref())
        .is_some_and(|(source, target)| {
            cast_compatibility::preserves_record_identity(source, target)
        })
    {
        return Ok(source);
    }
    Ok(target)
}

pub(super) fn binary_type_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let ScalarExpr::Binary { op, lhs, rhs } = expression else {
        unreachable!("binary expression");
    };
    let left = common::common_context_expression_type_with_control(
        lhs, schema, params, resolver, control,
    )?;
    let right = common::common_context_expression_type_with_control(
        rhs, schema, params, resolver, control,
    )?;
    // An operator's `unknown` literal is read by the input function of the operand type the selected operator declares, as `coerce_type` reads it when the operator is resolved: a `regclass` column compared with `'name'` reads the literal as `oid`, and `1 + '1'` reads `'1'` as an integer.
    if left.is_some() != right.is_some()
        && left
            .as_deref()
            .into_iter()
            .chain(right.as_deref())
            .all(|ty| {
                !matches!(
                    common::base_type(ty),
                    ColumnType::Vector(_) | ColumnType::Tensor(_)
                )
            })
    {
        let selected = operators::binary_operator_types_with_control(
            *op,
            left.as_deref(),
            right.as_deref(),
            control,
        )?;
        let [left_target, right_target, _] = &*selected;
        common::read_unknown_literals_with_control(
            std::iter::once(lhs.as_ref()),
            left_target,
            control,
        )?;
        common::read_unknown_literals_with_control(
            std::iter::once(rhs.as_ref()),
            right_target,
            control,
        )?;
    }
    operators::binary_result_type_with_control(*op, left.as_deref(), right.as_deref(), control)
}

pub(super) fn between_type_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let ScalarExpr::Between { expr, low, high } = expression else {
        unreachable!("between expression");
    };
    let value = common::common_context_expression_type_with_control(
        expr, schema, params, resolver, control,
    )?;
    let low = common::common_context_expression_type_with_control(
        low, schema, params, resolver, control,
    )?;
    let high = common::common_context_expression_type_with_control(
        high, schema, params, resolver, control,
    )?;
    operators::binary_result_type_with_control(
        crate::ast::BinaryOp::GreaterEqual,
        value.as_deref(),
        low.as_deref(),
        control,
    )?;
    operators::binary_result_type_with_control(
        crate::ast::BinaryOp::LessEqual,
        value.as_deref(),
        high.as_deref(),
        control,
    )?;
    copy_type(Some(&ColumnType::Boolean), control)
}

pub(super) fn membership_type_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let ScalarExpr::InList { expr, list, .. } = expression else {
        unreachable!("membership expression");
    };
    let needle = common::common_context_expression_type_with_control(
        expr, schema, params, resolver, control,
    )?;
    let mut candidates = ProductionVec::new(*control);
    candidates.reserve(list.len())?;
    for item in list {
        let ty = common::common_context_expression_type_with_control(
            item, schema, params, resolver, control,
        )?;
        candidates.push_produced(control.finish(ty, control.empty_reservation())?)?;
    }
    // `transformAExprIn` compares the needle with the list coerced to the inputs' common type; without one, each item is compared separately.
    let mut inputs = ProductionVec::new(*control);
    inputs.reserve(candidates.len() + 1)?;
    inputs.push_copy(needle.as_deref())?;
    for candidate in candidates.iter() {
        inputs.push_copy(candidate.as_deref())?;
    }
    let selected = common::select_common_input_type_with_control(&inputs, control)?;
    drop(inputs);
    match selected {
        Some(common) => {
            common::read_unknown_literals_with_control(
                std::iter::once(expr.as_ref()).chain(list.iter()),
                &common,
                control,
            )?;
            operators::binary_result_type_with_control(
                crate::ast::BinaryOp::Equal,
                needle.as_deref(),
                Some(&common),
                control,
            )?;
        }
        None => {
            for candidate in candidates.iter() {
                operators::binary_result_type_with_control(
                    crate::ast::BinaryOp::Equal,
                    needle.as_deref(),
                    candidate.as_deref(),
                    control,
                )?;
            }
        }
    }
    drop(candidates);
    drop(needle);
    copy_type(Some(&ColumnType::Boolean), control)
}

pub(super) fn conditional_type_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let ScalarExpr::Case {
        base,
        when,
        else_branch,
    } = expression
    else {
        unreachable!("conditional expression");
    };
    let simple = base.is_some();
    let base_type = base
        .as_deref()
        .map(|base| {
            common::common_context_expression_type_with_control(
                base, schema, params, resolver, control,
            )
        })
        .transpose()?
        .flatten();
    let mut results = ProductionVec::new(*control);
    results.reserve(when.len())?;
    for (condition, value) in when {
        let condition_type = if simple {
            common::common_context_expression_type_with_control(
                condition, schema, params, resolver, control,
            )?
        } else {
            scalar_type_inner_with_control(condition, schema, params, resolver, control)?
        };
        if simple {
            operators::binary_result_type_with_control(
                crate::ast::BinaryOp::Equal,
                base_type.as_deref(),
                condition_type.as_deref(),
                control,
            )?;
        }
        let ty = common::common_context_expression_type_with_control(
            value, schema, params, resolver, control,
        )?;
        results.push_produced(control.finish(ty, control.empty_reservation())?)?;
    }
    // `transformCaseExpr` selects the result type with the ELSE result first, including its implicit NULL.
    let else_type = else_branch
        .as_deref()
        .map(|value| {
            common::common_context_expression_type_with_control(
                value, schema, params, resolver, control,
            )
        })
        .transpose()?
        .flatten();
    if else_type.is_none()
        && results.iter().all(Option::is_none)
        && !when
            .iter()
            .map(|(_, value)| value)
            .chain(else_branch.as_deref())
            .all(crate::type_resolution::is_unknown_literal)
    {
        return Ok(None);
    }
    let mut selection = common::CommonInputType::new();
    let expressions =
        std::iter::once(else_branch.as_deref()).chain(when.iter().map(|(_, value)| Some(value)));
    // Each inferred type retains its own payload lease; keep only the vector's capacity lease alongside its consuming iterator.
    let (results, storage) = results.finish()?.into_parts();
    for (ty, expression) in std::iter::once(else_type).chain(results).zip(expressions) {
        if ty.is_some() || expression.is_none_or(common::unknown_input) {
            selection.include_owned(common::CommonTypeContext::Case, ty, control)?;
        }
    }
    drop(storage);
    let result = selection.finish(control)?;
    common::read_unknown_literals_with_control(
        when.iter()
            .map(|(_, value)| value)
            .chain(else_branch.as_deref()),
        &result,
        control,
    )?;
    common::case::case_output_type_with_control(
        expression,
        &result,
        &mut |expression| {
            scalar_type_inner_with_control(expression, schema, params, resolver, control)
        },
        control,
    )
    .map(Some)
}
