//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Function input validation shares the caller's original argument type observations.

mod observations;
pub(crate) use observations::CallInputTypes;

use super::{
    call::{infer_with_control, InferType},
    functions, FunctionTypeResolver,
};
use crate::{schema::ScalarTypeSchema, ColumnType, SQLError, SQLParam, ScalarExpr};
use uqa_core::memory::{Produced, ProductionControl};

pub(super) fn call_type_with_control(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
    include_window_specification: bool,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match expression {
        ScalarExpr::Func {
            order_syntax: _,
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => {
            if let Some(error) = binding
                .as_ref()
                .and_then(|binding| binding.resolution_error.as_ref())
            {
                return Err(error.sql_error());
            }
            if binding.as_ref().and_then(|binding| binding.dispatch)
                == Some(crate::ast::FunctionDispatch::FieldSelect)
            {
                if let Some(field) = binding
                    .as_ref()
                    .and_then(|binding| binding.composite_field())
                {
                    return super::inference::copy_type(Some(&field.result_type), control);
                }
                return super::inference::field_selection_type(
                    args, schema, params, resolver, control,
                );
            }
            if let Some(filter) = filter {
                infer_with_control(filter, infer, control)?;
            }
            if *distinct {
                for argument in args {
                    if let Some(ty) = infer_with_control(argument, infer, control)? {
                        super::ComparisonSupport::Equality.require(&ty, resolver, control)?;
                    }
                }
            }
            for order in order_by {
                if let Some(ty) = infer_with_control(&order.expr, infer, control)? {
                    super::ComparisonSupport::Ordering.require(&ty, resolver, control)?;
                }
            }
            if *distinct {
                for argument in args {
                    if let Some(ty) = infer_with_control(argument, infer, control)? {
                        super::ComparisonSupport::require_aggregate_ordering(
                            &ty, resolver, control,
                        )?;
                    }
                }
            }
            functions::builtin_function_type_with_control(
                functions::FunctionTypeCall {
                    name,
                    binding: binding.as_ref(),
                    args,
                },
                order_by,
                params,
                resolver,
                infer,
                control,
            )
        }
        ScalarExpr::WindowCall {
            name,
            args,
            spec,
            filter,
            ..
        } => {
            if let Some(filter) = filter {
                infer_with_control(filter, infer, control)?;
            }
            if include_window_specification {
                window_inputs(spec, resolver, infer, control)?;
            }
            functions::builtin_function_type_with_control(
                functions::FunctionTypeCall {
                    name,
                    binding: None,
                    args,
                },
                &[],
                params,
                resolver,
                infer,
                control,
            )
        }
        _ => Err(SQLError::Internal(
            "call type analysis requires a function".into(),
        )),
    }
}

/// Infer only the containing call; original child observations retain unknown and selected types.
pub(crate) fn call_type_from_observations_with_control(
    expression: &ScalarExpr,
    schema: &CallInputTypes<'_>,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    call_type_with_control(
        expression,
        schema,
        params,
        Some(resolver),
        &mut |expression| {
            let ty = schema.observed_expression_type(expression).ok_or_else(|| {
                SQLError::Internal("call type analysis requested an unobserved argument".into())
            })?;
            super::inference::copy_type(ty, control)
        },
        control,
        false,
    )
}

fn window_inputs(
    spec: &crate::ScalarWindowSpec,
    resolver: Option<&dyn FunctionTypeResolver>,
    infer: &mut InferType<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for expression in &spec.partition_by {
        if let Some(ty) = infer_with_control(expression, infer, control)? {
            super::ComparisonSupport::Equality.require(&ty, resolver, control)?;
        }
    }
    for order in &spec.order_by {
        if let Some(ty) = infer_with_control(&order.expr, infer, control)? {
            super::ComparisonSupport::Ordering.require(&ty, resolver, control)?;
        }
    }
    if let Some(frame) = &spec.frame {
        for bound in [&frame.start, &frame.end] {
            match bound {
                crate::ScalarFrameBound::Preceding(expression)
                | crate::ScalarFrameBound::Following(expression) => {
                    infer_with_control(expression, infer, control)?;
                }
                crate::ScalarFrameBound::UnboundedPreceding
                | crate::ScalarFrameBound::UnboundedFollowing
                | crate::ScalarFrameBound::CurrentRow => {}
            }
        }
    }
    Ok(())
}
