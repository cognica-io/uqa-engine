//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible grouping-set preparation after input schema binding.

use std::collections::HashSet;

use crate::ast::ColumnType;
use crate::{RowSchema, ScalarExpr, ScalarFrameBound};
use uqa_core::Value;

use crate::{plan::QueryBlockPlan, FunctionTypeResolver, SQLError, SQLParam};

mod expressions;
mod names;
pub(crate) use names::resolve_grouping_expression_reference;
pub use names::{bind_grouping_names, grouping_output_name, resolve_grouping_expression};
mod validation;
pub use validation::validate_grouped_expressions;
#[cfg(test)]
mod tests;

/// Resolve grouping names against the input before expanding output aliases and deduplicating `GROUP BY DISTINCT` sets. Equivalent qualified and unqualified columns share one identity without conflating expressions that `PostgreSQL` resolves to different operator inputs.
pub fn prepare_grouping_sets(
    engine: &dyn crate::routines::RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<QueryBlockPlan>, SQLError> {
    if statement.group_by.is_empty() && statement.grouping_sets.is_empty() {
        return Ok(None);
    }

    let mut prepared = statement.clone();
    let changed = prepare_grouping_sets_in_place(engine, &mut prepared, schema, None, params)?;
    Ok(changed.then_some(prepared))
}

/// Finalize analyzed grouping identity before scalar optimization can erase it.
pub(crate) fn prepare_grouping_sets_in_place(
    engine: &dyn crate::routines::RoutineResolution,
    prepared: &mut QueryBlockPlan,
    schema: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[SQLParam],
) -> Result<bool, SQLError> {
    let mut changed = bind_grouping_names(engine, prepared, schema, outer, params)?;
    changed |= expressions::bind_grouping_expressions(engine, prepared, schema, params)?;
    if !prepared.group_distinct {
        return Ok(changed);
    }
    analyze_distinct_grouping(engine, prepared, schema, outer, params)?.apply(prepared)?;
    Ok(true)
}

/// Pre-optimization grouping identities. The stored SQL keeps its original DISTINCT clause and sets; this selection belongs only to its analyzed executable copy.
#[derive(Debug, Clone)]
pub struct AnalyzedGroupingSets {
    original_count: usize,
    retained: Vec<usize>,
}

impl AnalyzedGroupingSets {
    pub(crate) fn copy_with(
        &self,
        copying: &mut crate::plan::copy::Copying<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            original_count: self.original_count,
            retained: copying.map(self.retained.iter(), |_, position| Ok(*position))?,
        })
    }

    /// Remove only sets that were equal before optimization, preserving multiplicity when distinct expressions later fold to the same value.
    pub fn apply(self, block: &mut QueryBlockPlan) -> Result<(), SQLError> {
        self.apply_with_retention(block, |sets, keep| {
            let mut index = 0;
            sets.retain(|_| {
                let retained = keep(index);
                index += 1;
                retained
            });
        })
    }

    pub(crate) fn apply_with_retention(
        self,
        block: &mut QueryBlockPlan,
        retain: impl FnOnce(&mut Vec<Vec<ScalarExpr>>, &mut dyn FnMut(usize) -> bool),
    ) -> Result<(), SQLError> {
        if !block.group_distinct || block.grouping_sets.len() != self.original_count {
            return Err(SQLError::Internal(
                "analyzed grouping selection lost its original sets".into(),
            ));
        }
        let mut retained = self.retained.into_iter().peekable();
        retain(&mut block.grouping_sets, &mut |index| {
            let keep = retained.peek() == Some(&index);
            if keep {
                retained.next();
            }
            keep
        });
        block.group_distinct = false;
        Ok(())
    }
}

pub(crate) fn analyze_distinct_grouping(
    engine: &dyn crate::routines::RoutineResolution,
    block: &QueryBlockPlan,
    schema: &RowSchema,
    outer: Option<&RowSchema>,
    params: &[SQLParam],
) -> Result<AnalyzedGroupingSets, SQLError> {
    let mut seen = HashSet::with_capacity(block.grouping_sets.len());
    let mut retained = Vec::with_capacity(block.grouping_sets.len());
    for (index, set) in block.grouping_sets.iter().enumerate() {
        let mut identity = set
            .iter()
            .map(|expression| {
                let expression = resolve_grouping_expression_reference(
                    engine,
                    expression,
                    &block.projections,
                    schema,
                    outer,
                    params,
                )?;
                expression_identity(engine, expression, schema, params)
            })
            .collect::<Result<Vec<_>, _>>()?;
        identity.sort_unstable();
        identity.dedup();
        if seen.insert(identity) {
            retained.push(index);
        }
    }
    Ok(AnalyzedGroupingSets {
        original_count: block.grouping_sets.len(),
        retained,
    })
}

pub(crate) fn expression_identity(
    engine: &dyn FunctionTypeResolver,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Vec<u8>, SQLError> {
    let expression =
        crate::bind_type_introspection_with_resolver(expression.clone(), schema, params, engine);
    let expression = normalize_expression(engine, expression, schema, params)?;
    serde_json::to_vec(&expression).map_err(|error| {
        SQLError::Internal(format!(
            "serialize GROUP BY DISTINCT expression identity: {error}"
        ))
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
fn normalize_expression(
    engine: &dyn FunctionTypeResolver,
    expression: ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<ScalarExpr, SQLError> {
    Ok(match expression {
        ScalarExpr::RetainedLiteral(literal) => {
            let normalized = normalize_expression(engine, literal.materialize()?, schema, params)?;
            match normalized {
                ScalarExpr::TypedLiteral {
                    bound_type: Some(ty),
                    ty: declaration,
                    collation_oid,
                    ..
                } => ScalarExpr::RetainedLiteral(crate::ir::RetainedScalarLiteral::new(
                    literal.value.clone(),
                    Some(ty),
                    Some(declaration),
                    collation_oid,
                )),
                ScalarExpr::Literal(_) => {
                    ScalarExpr::RetainedLiteral(crate::ir::RetainedScalarLiteral::new(
                        literal.value.clone(),
                        literal.ty.clone(),
                        None,
                        None,
                    ))
                }
                other => other,
            }
        }
        ScalarExpr::Column(column) => schema
            .column_slot(&column)
            .map_or(ScalarExpr::Column(column), ScalarExpr::Position),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            schema.qualified_slot(&qualifier, &column).map_or(
                ScalarExpr::QualifiedColumn { qualifier, column },
                ScalarExpr::Position,
            )
        }
        ScalarExpr::Func {
            order_syntax,
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => {
            let name = canonical_function_name(name);
            let argument_types = args
                .iter()
                .map(|argument| expression_type(engine, argument, schema, params))
                .collect::<Result<Vec<_>, _>>()?;
            let targets = crate::builtin_function_argument_targets(&name, &argument_types);
            ScalarExpr::Func {
                order_syntax,
                name,
                binding,
                args: args
                    .into_iter()
                    .zip(targets)
                    .map(|(argument, target)| {
                        normalize_unknown_literal(engine, argument, target.as_ref(), schema, params)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                distinct,
                order_by: order_by
                    .into_iter()
                    .map(|mut order| {
                        order.expr = normalize_expression(engine, order.expr, schema, params)?;
                        Ok(order)
                    })
                    .collect::<Result<Vec<_>, SQLError>>()?,
                filter: filter
                    .map(|expression| {
                        normalize_expression(engine, *expression, schema, params).map(Box::new)
                    })
                    .transpose()?,
            }
        }
        ScalarExpr::Array(items) => {
            ScalarExpr::Array(normalize_items(engine, items, schema, params)?)
        }
        ScalarExpr::Row(items) => ScalarExpr::Row(normalize_items(engine, items, schema, params)?),
        ScalarExpr::CompositeRow {
            items,
            binding,
            bound_type,
        } => ScalarExpr::CompositeRow {
            items: normalize_items(engine, items, schema, params)?,
            binding: binding.clone(),
            bound_type: bound_type.clone(),
        },
        ScalarExpr::Binary { op, lhs, rhs } => {
            let left_type = expression_type(engine, &lhs, schema, params)?;
            let right_type = expression_type(engine, &rhs, schema, params)?;
            ScalarExpr::Binary {
                op,
                lhs: Box::new(normalize_unknown_literal(
                    engine,
                    *lhs,
                    left_type.is_none().then_some(right_type.as_ref()).flatten(),
                    schema,
                    params,
                )?),
                rhs: Box::new(normalize_unknown_literal(
                    engine,
                    *rhs,
                    right_type.is_none().then_some(left_type.as_ref()).flatten(),
                    schema,
                    params,
                )?),
            }
        }
        ScalarExpr::UnaryMinus(expression) => {
            let expression = normalize_expression(engine, *expression, schema, params)?;
            if let ScalarExpr::Literal(
                value @ (Value::Int(_) | Value::Float(_) | Value::Decimal(_)),
            ) = &expression
            {
                ScalarExpr::Literal(crate::expr::negate_value(value, None)?)
            } else {
                ScalarExpr::UnaryMinus(Box::new(expression))
            }
        }
        ScalarExpr::Not(expression) => ScalarExpr::Not(Box::new(normalize_expression(
            engine,
            *expression,
            schema,
            params,
        )?)),
        ScalarExpr::And(items) => ScalarExpr::And(normalize_items(engine, items, schema, params)?),
        ScalarExpr::Or(items) => ScalarExpr::Or(normalize_items(engine, items, schema, params)?),
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(normalize_expression(engine, *expr, schema, params)?),
            negated,
        },
        ScalarExpr::Between { expr, low, high } => {
            let target = expression_type(engine, &expr, schema, params)?;
            ScalarExpr::Between {
                expr: Box::new(normalize_expression(engine, *expr, schema, params)?),
                low: Box::new(normalize_unknown_literal(
                    engine,
                    *low,
                    target.as_ref(),
                    schema,
                    params,
                )?),
                high: Box::new(normalize_unknown_literal(
                    engine,
                    *high,
                    target.as_ref(),
                    schema,
                    params,
                )?),
            }
        }
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => {
            let target = expression_type(engine, &expr, schema, params)?;
            ScalarExpr::InList {
                expr: Box::new(normalize_expression(engine, *expr, schema, params)?),
                list: list
                    .into_iter()
                    .map(|item| {
                        normalize_unknown_literal(engine, item, target.as_ref(), schema, params)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                negated,
            }
        }
        ScalarExpr::WindowCall {
            name,
            args,
            mut spec,
            filter,
            modifiers,
        } => {
            spec.partition_by = normalize_items(engine, spec.partition_by, schema, params)?;
            for order in &mut spec.order_by {
                order.expr = normalize_expression(engine, order.expr.clone(), schema, params)?;
            }
            if let Some(frame) = &mut spec.frame {
                normalize_frame_bound(engine, &mut frame.start, schema, params)?;
                normalize_frame_bound(engine, &mut frame.end, schema, params)?;
            }
            ScalarExpr::WindowCall {
                modifiers,
                name: canonical_function_name(name),
                args: normalize_items(engine, args, schema, params)?,
                spec,
                filter: filter
                    .map(|expression| {
                        normalize_expression(engine, *expression, schema, params).map(Box::new)
                    })
                    .transpose()?,
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: base
                .map(|expression| {
                    normalize_expression(engine, *expression, schema, params).map(Box::new)
                })
                .transpose()?,
            when: when
                .into_iter()
                .map(|(condition, result)| {
                    Ok((
                        normalize_expression(engine, condition, schema, params)?,
                        normalize_expression(engine, result, schema, params)?,
                    ))
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            else_branch: else_branch
                .map(|expression| {
                    normalize_expression(engine, *expression, schema, params).map(Box::new)
                })
                .transpose()?,
        },
        ScalarExpr::Collated {
            expr,
            ty,
            collation,
        } => {
            let mut expression = normalize_expression(engine, *expr, schema, params)?;
            match (&mut expression, collation.implicit_oid()) {
                (
                    ScalarExpr::TypedLiteral {
                        bound_type,
                        collation_oid,
                        ..
                    },
                    Some(oid),
                ) if bound_type.as_ref() == Some(&ty) => {
                    *collation_oid = Some(oid);
                    normalize_expression(engine, expression, schema, params)?
                }
                _ => ScalarExpr::Collated {
                    expr: Box::new(expression),
                    ty,
                    collation,
                },
            }
        }
        ScalarExpr::Cast { expr, ty, .. } => {
            let source_type = expression_type(engine, &expr, schema, params)?;
            let target_type = crate::type_resolution::resolve_declared_column_type(
                engine,
                &ColumnType::Named(ty),
            )?;
            let expression = normalize_expression(engine, *expr, schema, params)?;
            if source_type.as_ref() == Some(&target_type) {
                expression
            } else if input_requires_catalog(&target_type) {
                ScalarExpr::Cast {
                    implicit: false,
                    expr: Box::new(expression),
                    ty: target_type.catalog_name(),
                }
            } else if let ScalarExpr::Literal(value @ Value::Str(_)) = &expression {
                let input_type = if matches!(
                    target_type.without_temporal_modifiers(),
                    ColumnType::Interval
                ) {
                    target_type.clone()
                } else {
                    target_type.without_type_modifiers()
                };
                let value = crate::expr::cast_value(value, &input_type.sql_name())?;
                let input = normalize_expression(
                    engine,
                    ScalarExpr::TypedLiteral {
                        collation_oid: None,
                        composite_source: None,
                        value,
                        ty: input_type.catalog_name(),
                        bound_type: Some(input_type.clone()),
                        parameter_index: None,
                    },
                    schema,
                    params,
                )?;
                if input_type == target_type {
                    input
                } else {
                    ScalarExpr::Cast {
                        implicit: false,
                        expr: Box::new(input),
                        ty: target_type.catalog_name(),
                    }
                }
            } else if let ScalarExpr::Literal(Value::Null) = expression {
                ScalarExpr::TypedLiteral {
                    collation_oid: None,
                    composite_source: None,
                    value: Value::Null,
                    ty: target_type.catalog_name(),
                    bound_type: Some(target_type),
                    parameter_index: None,
                }
            } else {
                ScalarExpr::Cast {
                    implicit: false,
                    expr: Box::new(expression),
                    ty: target_type.catalog_name(),
                }
            }
        }
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(normalize_expression(engine, *expr, schema, params)?),
            subquery,
            negated,
        },
        ScalarExpr::TypedLiteral {
            value,
            ty,
            bound_type,
            parameter_index,
            collation_oid,
            composite_source,
        } => {
            let literal = ScalarExpr::Literal(value.clone());
            let declared = match bound_type {
                Some(ty) => ty,
                None => crate::type_resolution::resolve_declared_column_type(
                    engine,
                    &ColumnType::Named(ty),
                )?,
            };
            let collation_oid = collation_oid.filter(|oid| {
                i64::from(*oid) != crate::catalog::type_metadata::pg_type_collation_oid(&declared)
            });
            if parameter_index.is_none()
                && composite_source.is_none()
                && collation_oid.is_none()
                && expression_type(engine, &literal, schema, params)?.as_ref() == Some(&declared)
            {
                literal
            } else {
                ScalarExpr::TypedLiteral {
                    collation_oid,
                    composite_source,
                    value,
                    ty: declared.catalog_name(),
                    bound_type: Some(declared),
                    parameter_index,
                }
            }
        }
        expression @ (ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }) => expression,
    })
}

fn input_requires_catalog(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Named(_)
        | ColumnType::Domain { .. }
        | ColumnType::Enum(_)
        | ColumnType::Composite(_)
        | ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regclass
        | ColumnType::Regcollation
        | ColumnType::Regnamespace
        | ColumnType::Regrole
        | ColumnType::Regtype
        | ColumnType::Record
        | ColumnType::AnyArray => true,
        ColumnType::Array(element) => input_requires_catalog(element),
        _ => false,
    }
}

fn normalize_items(
    engine: &dyn FunctionTypeResolver,
    items: Vec<ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Vec<ScalarExpr>, SQLError> {
    items
        .into_iter()
        .map(|item| normalize_expression(engine, item, schema, params))
        .collect()
}

fn normalize_unknown_literal(
    engine: &dyn FunctionTypeResolver,
    expression: ScalarExpr,
    target: Option<&ColumnType>,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<ScalarExpr, SQLError> {
    if matches!(expression, ScalarExpr::Literal(Value::Null)) {
        if let Some(target) = target {
            return normalize_expression(
                engine,
                ScalarExpr::Cast {
                    implicit: true,
                    expr: Box::new(expression),
                    ty: target.catalog_name(),
                },
                schema,
                params,
            );
        }
    }
    normalize_expression(engine, expression, schema, params)
}

fn normalize_frame_bound(
    engine: &dyn FunctionTypeResolver,
    bound: &mut ScalarFrameBound,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    match bound {
        ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
            **expression = normalize_expression(engine, (**expression).clone(), schema, params)?;
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => {}
    }
    Ok(())
}

fn expression_type(
    engine: &dyn FunctionTypeResolver,
    expression: &ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<ColumnType>, SQLError> {
    crate::scalar_type_with_resolver(expression, schema, params, engine)
}

fn canonical_function_name(name: String) -> String {
    let lower = name.to_ascii_lowercase();
    match lower.strip_prefix("pg_catalog.") {
        Some(unqualified) => unqualified.to_owned(),
        None => lower,
    }
}

impl uqa_core::memory::ObserveAllocations for AnalyzedGroupingSets {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        let Self {
            original_count: _,
            retained,
        } = self;
        observer.child(retained)?;
        Ok(())
    }
}
