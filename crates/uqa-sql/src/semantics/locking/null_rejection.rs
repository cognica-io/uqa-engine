//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Determine nullable join sides from SQL predicates and strict function metadata.

use crate::semantics::{join_alias_input_schemas_with_control, resolve_join_using_with_control};
use crate::{
    binding::{
        context::BindingContext,
        production::{bind_source_plan_schema_in_scope_with_control, ControlledBindingContext},
        SourceScope,
    },
    plan::SourcePlan,
    routines::RoutineResolution,
    RowSchema, SQLError, SQLParam, ScalarExpr,
};
use uqa_core::{
    memory::{ProductionControl, ProductionVec},
    Value,
};

pub fn reduce_null_rejected_outer_joins_to_fixpoint(
    routines: &dyn RoutineResolution,
    source: &mut SourcePlan,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    context: &BindingContext<'_>,
) -> Result<(), SQLError> {
    let control = ProductionControl::uncontrolled();
    reduce_null_rejected_outer_joins_with_control(
        routines,
        source,
        predicate,
        params,
        &context.copy_with_control(&control)?,
        &control,
    )
}

pub(crate) fn reduce_null_rejected_outer_joins_with_control(
    routines: &dyn RoutineResolution,
    source: &mut SourcePlan,
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    context: &ControlledBindingContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    loop {
        control.check()?;
        let mut qualifications = ProductionVec::new(*control);
        if let Some(predicate) = predicate {
            qualifications.push_produced(predicate.clone_with_control(control)?)?;
        }
        collect_inner_join_qualifications(source, &mut qualifications, control)?;
        let mut changed = false;
        for qualification in qualifications.iter() {
            changed |= reduce_null_rejected_outer_joins(
                routines,
                source,
                qualification,
                params,
                context,
                SourceScope::new(None),
                control,
            )?;
        }
        if !changed {
            return Ok(());
        }
    }
}

fn collect_inner_join_qualifications(
    source: &SourcePlan,
    output: &mut ProductionVec<'_, ScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let SourcePlan::Join {
        left,
        right,
        kind,
        on,
        ..
    } = source
    else {
        return Ok(());
    };
    if !matches!(
        kind,
        crate::ast::JoinKind::Inner | crate::ast::JoinKind::Cross
    ) {
        return Ok(());
    }
    if let Some(on) = on {
        output.push_produced(on.clone_with_control(control)?)?;
    }
    collect_inner_join_qualifications(left, output, control)?;
    collect_inner_join_qualifications(right, output, control)
}

fn reduce_null_rejected_outer_joins(
    routines: &dyn RoutineResolution,
    source: &mut SourcePlan,
    predicate: &ScalarExpr,
    params: &[SQLParam],
    context: &ControlledBindingContext<'_>,
    scope: SourceScope<'_>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let SourcePlan::Join {
        left,
        right,
        kind,
        using,
        natural,
        alias,
        column_aliases,
        lateral,
        ..
    } = source
    else {
        return Ok(false);
    };
    let left_schema = bind_source_plan_schema_in_scope_with_control(
        routines,
        left,
        params,
        context.copy_with_control(control)?,
        scope.with_names(),
        control,
    )?;
    let preceding = scope.right_namespace_with_control(left, &left_schema, *kind, control)?;
    let right_scope = SourceScope {
        outer: scope.outer,
        preceding: Some(&preceding),
        inherited_lateral: *lateral,
        retain_names: true,
    };
    let right_schema = bind_source_plan_schema_in_scope_with_control(
        routines,
        right,
        params,
        context.copy_with_control(control)?,
        right_scope,
        control,
    )?;
    let resolved_using = resolve_join_using_with_control(
        using.as_ref(),
        *natural,
        &left_schema,
        &right_schema,
        control,
    )?;
    let (left_predicate_schema, right_predicate_schema) = match alias.as_deref() {
        Some(alias) => join_alias_input_schemas_with_control(
            *kind,
            &left_schema,
            &right_schema,
            resolved_using.as_deref(),
            alias,
            column_aliases,
            control,
        )?,
        None => (left_schema.clone(), right_schema.clone()),
    };
    let rejects_left = predicate_rejects_null_extended_side(
        routines,
        predicate,
        &left_predicate_schema,
        &right_predicate_schema,
        params,
        control,
    )?;
    let rejects_right = predicate_rejects_null_extended_side(
        routines,
        predicate,
        &right_predicate_schema,
        &left_predicate_schema,
        params,
        control,
    )?;
    let reduced = match (*kind, rejects_left, rejects_right) {
        (crate::ast::JoinKind::Left, _, true) | (crate::ast::JoinKind::Right, true, _) => {
            crate::ast::JoinKind::Inner
        }
        (crate::ast::JoinKind::Full, true, true) => crate::ast::JoinKind::Inner,
        (crate::ast::JoinKind::Full, true, false) => crate::ast::JoinKind::Left,
        (crate::ast::JoinKind::Full, false, true) => crate::ast::JoinKind::Right,
        (kind, _, _) => kind,
    };
    let mut changed = reduced != *kind;
    *kind = reduced;
    changed |= reduce_null_rejected_outer_joins(
        routines, left, predicate, params, context, scope, control,
    )?;
    changed |= reduce_null_rejected_outer_joins(
        routines,
        right,
        predicate,
        params,
        context,
        right_scope,
        control,
    )?;
    Ok(changed)
}

const TRUTH_FALSE: u8 = 1;
const TRUTH_TRUE: u8 = 2;
const TRUTH_NULL: u8 = 4;
const TRUTH_ANY: u8 = TRUTH_FALSE | TRUTH_TRUE | TRUTH_NULL;

fn predicate_rejects_null_extended_side(
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    side: &RowSchema,
    other: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    Ok(
        truth_values_with_null_side(routines, expression, side, other, params, control)?
            & TRUTH_TRUE
            == 0,
    )
}

fn truth_values_with_null_side(
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    side: &RowSchema,
    other: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<u8, SQLError> {
    control.check()?;
    match expression {
        ScalarExpr::Literal(Value::Bool(value)) => {
            if *value {
                Ok(TRUTH_TRUE)
            } else {
                Ok(TRUTH_FALSE)
            }
        }
        ScalarExpr::Literal(Value::Null) => Ok(TRUTH_NULL),
        ScalarExpr::IsNull { expr, negated }
            if expression_is_null_with_side(routines, expr, side, other, params, control)? =>
        {
            if *negated {
                Ok(TRUTH_FALSE)
            } else {
                Ok(TRUTH_TRUE)
            }
        }
        ScalarExpr::Between { expr, low, high } => {
            let value_is_null =
                expression_is_null_with_side(routines, expr, side, other, params, control)?;
            let low_is_null =
                expression_is_null_with_side(routines, low, side, other, params, control)?;
            let high_is_null =
                expression_is_null_with_side(routines, high, side, other, params, control)?;
            if value_is_null || (low_is_null && high_is_null) {
                Ok(TRUTH_NULL)
            } else if low_is_null || high_is_null {
                Ok(TRUTH_FALSE | TRUTH_NULL)
            } else {
                Ok(TRUTH_ANY)
            }
        }
        expression
            if expression_is_null_with_side(
                routines, expression, side, other, params, control,
            )? =>
        {
            Ok(TRUTH_NULL)
        }
        ScalarExpr::Not(inner) => Ok(negate_truth_values(truth_values_with_null_side(
            routines, inner, side, other, params, control,
        )?)),
        ScalarExpr::And(items) => items.iter().try_fold(TRUTH_TRUE, |left, right| {
            Ok(combine_truth_values(
                left,
                truth_values_with_null_side(routines, right, side, other, params, control)?,
                true,
            ))
        }),
        ScalarExpr::Or(items) => items.iter().try_fold(TRUTH_FALSE, |left, right| {
            Ok(combine_truth_values(
                left,
                truth_values_with_null_side(routines, right, side, other, params, control)?,
                false,
            ))
        }),
        _ => Ok(TRUTH_ANY),
    }
}

fn expression_is_null_with_side(
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    side: &RowSchema,
    other: &RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    match expression {
        ScalarExpr::Literal(Value::Null) => Ok(true),
        ScalarExpr::Column(column) => Ok(side.unqualified_position(column).is_some()
            && other.unqualified_position(column).is_none()),
        ScalarExpr::QualifiedColumn { qualifier, column } => Ok(side
            .has_qualified_column(qualifier, column)
            && !other.has_qualified_column(qualifier, column)),
        ScalarExpr::Binary { lhs, rhs, .. } => Ok(expression_is_null_with_side(
            routines, lhs, side, other, params, control,
        )? || expression_is_null_with_side(
            routines, rhs, side, other, params, control,
        )?),
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => {
            expression_is_null_with_side(routines, inner, side, other, params, control)
        }
        ScalarExpr::Between { expr, low, high } => Ok(expression_is_null_with_side(
            routines, expr, side, other, params, control,
        )? || (expression_is_null_with_side(
            routines, low, side, other, params, control,
        )? && expression_is_null_with_side(
            routines, high, side, other, params, control,
        )?)),
        ScalarExpr::InList { expr, .. } => {
            expression_is_null_with_side(routines, expr, side, other, params, control)
        }
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            if !scalar_function_is_strict(
                routines,
                name,
                binding.as_ref(),
                args,
                (side, other),
                params,
                control,
            )? {
                return Ok(false);
            }
            for argument in crate::ir::scalar_call_arguments_with_control(args, control)?.iter() {
                if expression_is_null_with_side(
                    routines,
                    argument.value,
                    side,
                    other,
                    params,
                    control,
                )? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn scalar_function_is_strict(
    routines: &dyn RoutineResolution,
    name: &str,
    binding: Option<&crate::ast::FunctionBinding>,
    args: &[ScalarExpr],
    sides: (&RowSchema, &RowSchema),
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let (side, other) = sides;
    let runtime_name = control.copy_text(name.strip_prefix("pg_catalog.").unwrap_or(name))?;
    let (mut runtime_name, memory) = runtime_name.into_parts();
    runtime_name.make_ascii_lowercase();
    let runtime_name = control.finish(runtime_name, memory)?;
    if routines.has_registered_scalar_function_with_control(&runtime_name, control)? {
        return Ok(false);
    }
    if let Some(strict) = crate::expr::bound_scalar_function_strictness(name, binding, args.len()) {
        return Ok(strict);
    }
    let schema = RowSchema::join_with_control(side, other, std::iter::empty(), control)?;
    let signature = crate::function_call_argument_signature_with_control(
        args,
        &schema,
        params,
        Some(routines),
        control,
    )?;
    if binding.is_none() && !routines.has_visible_sql_functions_with_control(name, control)? {
        return Ok(false);
    }
    Ok(routines
        .resolve_static_sql_function_strictness_with_control(
            name,
            binding,
            crate::type_resolution::RoutineCallDescriptor {
                argument_names: &signature.0,
                argument_types: &signature.1,
                explicit_variadic: signature.2,
            },
            control,
        )?
        .unwrap_or(false))
}

fn negate_truth_values(values: u8) -> u8 {
    (u8::from(values & TRUTH_FALSE != 0) * TRUTH_TRUE)
        | (u8::from(values & TRUTH_TRUE != 0) * TRUTH_FALSE)
        | (values & TRUTH_NULL)
}

fn combine_truth_values(left: u8, right: u8, and: bool) -> u8 {
    let mut output = 0;
    for lhs in [TRUTH_FALSE, TRUTH_TRUE, TRUTH_NULL] {
        if left & lhs == 0 {
            continue;
        }
        for rhs in [TRUTH_FALSE, TRUTH_TRUE, TRUTH_NULL] {
            if right & rhs == 0 {
                continue;
            }
            output |= if and {
                match (lhs, rhs) {
                    (TRUTH_FALSE, _) | (_, TRUTH_FALSE) => TRUTH_FALSE,
                    (TRUTH_TRUE, TRUTH_TRUE) => TRUTH_TRUE,
                    _ => TRUTH_NULL,
                }
            } else {
                match (lhs, rhs) {
                    (TRUTH_TRUE, _) | (_, TRUTH_TRUE) => TRUTH_TRUE,
                    (TRUTH_FALSE, TRUTH_FALSE) => TRUTH_FALSE,
                    _ => TRUTH_NULL,
                }
            };
        }
    }
    output
}
