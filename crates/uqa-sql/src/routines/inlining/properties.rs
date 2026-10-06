//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volatility, strictness and repeated-argument cost of analyzed expressions.

use super::RoutineInliningContext;
use crate::{
    ast::{CreateFunction, FunctionVolatility},
    ColumnType, RowSchema, SQLError, SQLParam, ScalarExpr,
};

pub(super) struct Properties {
    volatility: FunctionVolatility,
    nonstrict: bool,
    subquery: bool,
    set_returning: bool,
    cost: f64,
}

impl Properties {
    pub(super) fn permits(&self, definition: &CreateFunction) -> bool {
        !self.subquery
            && !self.set_returning
            && rank(self.volatility) <= rank(definition.volatility)
            && (!definition.strict || !self.nonstrict)
    }
}

const fn rank(volatility: FunctionVolatility) -> u8 {
    match volatility {
        FunctionVolatility::Immutable => 0,
        FunctionVolatility::Stable => 1,
        FunctionVolatility::Volatile => 2,
    }
}

pub(super) fn can_duplicate(
    context: &RoutineInliningContext<'_>,
    expression: &ScalarExpr,
) -> Result<bool, SQLError> {
    let properties = inspect(context, expression, &[])?;
    Ok(!properties.subquery
        && properties.cost <= 10.0
        && properties.volatility != FunctionVolatility::Volatile)
}

pub(super) fn inspect(
    context: &RoutineInliningContext<'_>,
    expression: &ScalarExpr,
    parameters: &[SQLParam],
) -> Result<Properties, SQLError> {
    let mut properties = Properties {
        volatility: FunctionVolatility::Immutable,
        nonstrict: false,
        subquery: false,
        set_returning: false,
        cost: 0.0,
    };
    let mut failure = None;
    expression.visit(&mut |node| {
        if failure.is_some() {
            return;
        }
        if let Err(error) = inspect_node(context, node, parameters, &mut properties) {
            failure = Some(error);
        }
    });
    failure.map_or(Ok(properties), Err)
}

fn inspect_node(
    context: &RoutineInliningContext<'_>,
    expression: &ScalarExpr,
    parameters: &[SQLParam],
    properties: &mut Properties,
) -> Result<(), SQLError> {
    let mut volatility = FunctionVolatility::Immutable;
    match expression {
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            volatility = inspect_call(context, name, binding.as_ref(), args.len(), properties);
        }
        ScalarExpr::Cast { expr, ty } => {
            let target = context.types.resolve_catalog_column_type_name(ty)?;
            let source = crate::scalar_type_with_resolver(
                expr,
                &RowSchema::default(),
                parameters,
                context.routines,
            )
            .ok()
            .flatten();
            // A caller column is not in the body's row scope. Its cast is
            // nevertheless at most stable; casts do not invoke volatile calls.
            volatility = source
                .as_ref()
                .map_or(FunctionVolatility::Stable, |source| {
                    crate::type_resolution::cast_volatility(source, &target)
                });
            properties.nonstrict |= matches!(target, ColumnType::Domain { .. });
            if source.as_ref() != Some(&target) {
                properties.cost += 1.0;
            }
        }
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => {
            properties.subquery = true;
            properties.nonstrict = true;
        }
        ScalarExpr::WindowCall { .. } => {
            properties.set_returning = true;
            properties.nonstrict = true;
        }
        ScalarExpr::Array(_)
        | ScalarExpr::Row(_)
        | ScalarExpr::And(_)
        | ScalarExpr::Or(_)
        | ScalarExpr::IsNull { .. }
        | ScalarExpr::Case { .. } => properties.nonstrict = true,
        ScalarExpr::Binary { op, lhs, rhs } => {
            volatility = super::operators::volatility(context, *op, lhs, rhs, parameters);
            properties.cost += 1.0;
        }
        ScalarExpr::Between { expr, low, high } => {
            properties.nonstrict = true; // PostgreSQL analyzes BETWEEN as AND.
            volatility = super::operators::comparisons(
                context,
                crate::ast::BinaryOp::LessEqual,
                [
                    (low.as_ref(), expr.as_ref()),
                    (expr.as_ref(), high.as_ref()),
                ],
                parameters,
            );
            properties.cost += 2.0;
        }
        ScalarExpr::InList { expr, list, .. } => {
            // More than one element is an ArrayExpr or an OR expression at
            // this point in parse analysis; both are nonstrict constructs.
            properties.nonstrict |= list.len() > 1;
            volatility = super::operators::comparisons(
                context,
                crate::ast::BinaryOp::Equal,
                list.iter().map(|item| (expr.as_ref(), item)),
                parameters,
            );
            properties.cost += if list.len() > 1 {
                list.len() as f64 * 0.5
            } else {
                1.0
            };
        }
        ScalarExpr::UnaryMinus(_) => properties.cost += 1.0,
        _ => {}
    }
    if rank(volatility) > rank(properties.volatility) {
        properties.volatility = volatility;
    }
    Ok(())
}

fn inspect_call(
    context: &RoutineInliningContext<'_>,
    name: &str,
    binding: Option<&crate::ast::FunctionBinding>,
    arity: usize,
    properties: &mut Properties,
) -> FunctionVolatility {
    let function = binding
        .filter(|binding| !binding.builtin)
        .and_then(|binding| {
            context
                .routines
                .lookup_bound_sql_functions_by_binding(binding)
                .and_then(|functions| {
                    functions
                        .into_iter()
                        .find(|function| function.def.object_id == binding.object_id)
                })
        });
    if let Some(function) = function {
        let volatility = function.def.volatility;
        properties.nonstrict |= !function.def.strict;
        properties.set_returning |= function.def.returns_set();
        properties.cost += f64::from(function.def.cost.unwrap_or(100.0));
        volatility
    } else {
        let volatility = crate::semantics::volatility::function_volatility_with_binding(
            context.volatility,
            name,
            binding,
            arity,
        );
        properties.nonstrict |=
            crate::expr::bound_scalar_function_strictness(name, binding, arity) != Some(true);
        properties.set_returning |= crate::semantics::sets::validation::builtin_returns_set(
            &crate::semantics::builtin_function_dispatch_name(&name.to_ascii_lowercase()),
        );
        // Internal routines have the catalog's default cost of one
        // cpu_operator_cost. Unclassified host callbacks are expensive.
        properties.cost += if context.routines.has_registered_scalar_function(name)
            && binding.is_none_or(|binding| !binding.builtin)
        {
            100.0
        } else {
            1.0
        };
        volatility
    }
}
