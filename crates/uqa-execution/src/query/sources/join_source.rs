//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for lateral, hash, and nested-loop joins.

/// Build the physical operator for a join source.
use super::{
    alias_join_operator, bind_source_plan_schema, build_join_operator_with_ctes_at_path,
    combine_filters, join_using_predicate, null_row_for_schema, physical_exec_error,
    physical_work_mem_bytes, qualifier_filter, require_lateral_function_execute,
    resolve_join_using, shape_join_using_output, unproduced_column, validate_join_on_schema,
    ColumnPrune, CteScope, JoinExecutionStrategy, PhysicalOperator, QualifierFilters,
    QueryLateralSource, RowSchemaExecution, SQLError, SQLParam, SourceContext, SourcePlan,
};
use crate::query::binding::sources::SourceBindings;
use crate::{HashJoin, LateralJoin, NestedLoopJoin};
use uqa_sql::routines::invocation::descriptions::RoutineCallDescriptions;
use uqa_sql::semantics::from_qualifier_set;

#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "forwards source binding metadata and tuple recheck paths"
)]
pub(super) fn build_join_source_operator<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    recheck_path: Option<Vec<u8>>,
    bindings: Option<&SourceBindings<'_>>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    match from {
        SourcePlan::Join {
            left,
            right,
            kind,
            on,
            using,
            natural,
            alias,
            column_aliases,
            lateral,
            strategy,
            ..
        } => {
            // A join alias hides the joined relations from the enclosing query block. The projections and predicates that block placed by relation name therefore cannot address any relation inside the alias, including one that reuses an outer name; conjuncts on the alias itself stay in the block's residual filter.
            let (prune, filters) = if alias.is_some() {
                (None, None)
            } else {
                (prune, filters)
            };
            let left_recheck_path = recheck_path.as_ref().map(|path| {
                let mut path = path.clone();
                path.push(0);
                path
            });
            let right_recheck_path = recheck_path.as_ref().map(|path| {
                let mut path = path.clone();
                path.push(1);
                path
            });
            let left_filters = filters.and_then(|filters| {
                context
                    .planning
                    .propagated_join_filters(filters, right, left, on.as_ref())
            });
            let left_filter_ref = left_filters.as_ref().or(filters);
            let left_operator = if context.planning.join_input_is_empty(from, false) {
                super::empty_source_operator(context, left, params, ctes, None)?
            } else {
                build_join_operator_with_ctes_at_path(
                    context,
                    left,
                    params,
                    ctes,
                    prune,
                    left_filter_ref,
                    left_recheck_path,
                    bindings
                        .and_then(|bindings| bindings.join_child(false))
                        .as_ref(),
                )?
            };
            let implicit_lateral_function = match right.as_ref() {
                SourcePlan::Function { name, .. } => {
                    let identity = name.to_ascii_lowercase();
                    let lower = uqa_sql::semantics::builtin_function_dispatch_name(&identity);
                    !uqa_sql::registry::is_operator_join_table_function(&lower)
                }
                SourcePlan::FunctionGroup { .. } => true,
                _ => false,
            };
            let right_filters = filters.and_then(|filters| {
                context
                    .planning
                    .propagated_join_filters(filters, left, right, on.as_ref())
            });
            let right_filter_ref = right_filters.as_ref().or(filters);
            let lateral_input = (*lateral
                || implicit_lateral_function
                || matches!(
                    right.as_ref(),
                    SourcePlan::Join { .. }
                        | SourcePlan::Values { lateral: true, .. }
                        | SourcePlan::Subquery { lateral: true, .. }
                ))
            .then(|| {
                ctes.row_lock_outer_row().map_or_else(
                    || left_operator.row_schema().clone(),
                    |outer| {
                        crate::RowSchema::with_outer_schema(
                            left_operator.row_schema(),
                            &outer.schema,
                        )
                    },
                )
            });
            let right_empty = context.planning.join_input_is_empty(from, true);
            let dependent = match lateral_input.as_ref().filter(|_| !right_empty) {
                Some(input) => crate::query::binding::source_depends_on_current_row(
                    context.ctes.routines,
                    right,
                    input,
                    ctes,
                )?,
                None => false,
            };
            if dependent {
                if !matches!(strategy, JoinExecutionStrategy::Auto) {
                    return Err(SQLError::Internal(
                        "optimizer selected a hash strategy for a lateral join".into(),
                    ));
                }
                let left_schema = left_operator.row_schema().clone();
                let input_schema = lateral_input.expect("dependent source has a calling scope");
                let left_nulls = null_row_for_schema(left_schema.columns());
                let right_schema = bind_source_plan_schema(
                    context.ctes.routines,
                    right,
                    params,
                    ctes,
                    Some(&input_schema),
                )?;
                require_lateral_function_execute(context, right, &input_schema, params, ctes)?;
                validate_join_on_schema(
                    context,
                    on.as_ref(),
                    &left_schema,
                    &right_schema,
                    params,
                    ctes,
                )?;
                let right_nulls = null_row_for_schema(right_schema.columns());
                let resolved_using =
                    resolve_join_using(using.as_ref(), *natural, &left_schema, &right_schema)?;
                let effective_on = resolved_using
                    .as_ref()
                    .and_then(|using| join_using_predicate(using, &left_schema, &right_schema))
                    .or_else(|| on.clone());
                if *kind == uqa_sql::ast::JoinKind::Full {
                    super::join_keys::join_keys(
                        *kind,
                        effective_on.as_ref(),
                        &left_schema,
                        &right_schema,
                        params,
                        context.types,
                        context.volatility,
                    )?;
                }
                let pinned_right = right_recheck_path
                    .as_deref()
                    .and_then(|path| ctes.recheck_source_row(path))
                    .map(|source| {
                        source
                            .schema
                            .relayout_physical_row(source.row, &right_schema)
                            .map(|row| crate::OwnedPhysicalRow::new(right_schema.clone(), row))
                            .map_err(physical_exec_error)
                    })
                    .transpose()?;
                let specialized_right = if pinned_right.is_none() {
                    super::output_filter::specialize_lateral_source(
                        context,
                        right,
                        right_filter_ref,
                    )?
                } else {
                    None
                };
                let right_filter_ref = if specialized_right.is_some() {
                    None
                } else {
                    right_filter_ref
                };
                // A predicate that cannot move inside the right query still applies to each produced row, including a tuple supplied by a lock recheck.
                let right_qualifiers = from_qualifier_set(right);
                let right_filter = combine_filters(
                    right_qualifiers
                        .iter()
                        .filter_map(|qualifier| qualifier_filter(right_filter_ref, qualifier)),
                )
                .map(|predicate| {
                    if let Some(column) = right_qualifiers.iter().find_map(|qualifier| {
                        unproduced_column(&predicate, qualifier, &right_schema)
                    }) {
                        return Err(column);
                    }
                    let evaluator = context.relational.evaluator(params, ctes);
                    let predicate = evaluator.bind_type_introspection(predicate, &right_schema);
                    Ok((
                        crate::scalar::PreparedExpressions::scalar(predicate),
                        evaluator,
                    ))
                });
                let source = QueryLateralSource {
                    context: *context,
                    right: specialized_right.unwrap_or_else(|| (**right).clone()),
                    right_filter,
                    on: effective_on.map(|expression| {
                        let joined = crate::RowSchema::join(
                            &left_schema,
                            &right_schema,
                            left_nulls.keys().chain(right_nulls.keys()).cloned(),
                        );
                        crate::scalar::PreparedExpressions::scalar(
                            context
                                .relational
                                .evaluator(params, ctes)
                                .bind_type_introspection(expression, &joined),
                        )
                    }),
                    function_arguments: match right.as_ref() {
                        SourcePlan::Function { args, .. } => {
                            Some(crate::scalar::PreparedExpressions::scalars(
                                args.iter()
                                    .map(|argument| {
                                        context
                                            .relational
                                            .evaluator(params, ctes)
                                            .bind_type_introspection(
                                                argument.clone(),
                                                &input_schema,
                                            )
                                    })
                                    .collect(),
                            ))
                        }
                        _ => None,
                    },
                    routine_descriptions: RoutineCallDescriptions::default(),
                    expression_runtime: std::sync::OnceLock::new(),
                    params,
                    ctes: ctes.clone(),
                    input_schema,
                    right_schema: right_schema.clone(),
                    pinned_right,
                };
                let joined: Box<dyn PhysicalOperator + 'a> =
                    Box::new(LateralJoin::new_with_right_schema(
                        left_operator,
                        Box::new(source),
                        *kind,
                        left_nulls,
                        right_nulls,
                        right_schema.clone(),
                    ));
                let joined = if let Some(using) = resolved_using.as_ref() {
                    shape_join_using_output(joined, *kind, &left_schema, &right_schema, using)
                } else {
                    Ok(joined)
                }?;
                return alias_join_operator(joined, alias.as_deref(), column_aliases);
            }

            let right_operator = if right_empty {
                super::empty_source_operator(context, right, params, ctes, lateral_input.as_ref())?
            } else {
                build_join_operator_with_ctes_at_path(
                    context,
                    right,
                    params,
                    ctes,
                    prune,
                    right_filter_ref,
                    right_recheck_path,
                    bindings
                        .and_then(|bindings| bindings.join_child(true))
                        .as_ref(),
                )?
            };

            let left_schema = left_operator.row_schema().clone();
            let right_schema = right_operator.row_schema().clone();
            validate_join_on_schema(
                context,
                on.as_ref(),
                &left_schema,
                &right_schema,
                params,
                ctes,
            )?;
            let resolved_using =
                resolve_join_using(using.as_ref(), *natural, &left_schema, &right_schema)?;
            let effective_on = resolved_using
                .as_ref()
                .and_then(|using| join_using_predicate(using, &left_schema, &right_schema))
                .or_else(|| on.clone());

            let evaluator = context.relational.evaluator(params, ctes);
            let effective_on = effective_on.map(|predicate| {
                let schema = crate::RowSchema::join(&left_schema, &right_schema, []);
                evaluator.bind_type_introspection(predicate, &schema)
            });
            let hash_plan = super::join_keys::join_keys(
                *kind,
                effective_on.as_ref(),
                &left_schema,
                &right_schema,
                params,
                context.types,
                context.volatility,
            )?;
            let left_nulls = null_row_for_schema(left_operator.schema());
            let right_nulls = null_row_for_schema(right_operator.schema());
            let work_mem = physical_work_mem_bytes(context.relational.runtime)?;
            let joined: Box<dyn PhysicalOperator + 'a> = match (strategy, hash_plan) {
                (JoinExecutionStrategy::Auto | JoinExecutionStrategy::Hash, Some(keys)) => {
                    Box::new(
                        HashJoin::try_new_with_work_mem_and_predicate(
                            left_operator,
                            right_operator,
                            *kind,
                            keys.left,
                            keys.right,
                            keys.residual,
                            evaluator,
                            left_nulls,
                            right_nulls,
                            work_mem,
                            params,
                        )
                        .map_err(physical_exec_error)?,
                    )
                }
                (JoinExecutionStrategy::Auto | JoinExecutionStrategy::Hash, None) => {
                    Box::new(NestedLoopJoin::new_with_work_mem(
                        left_operator,
                        right_operator,
                        *kind,
                        effective_on,
                        evaluator,
                        left_nulls,
                        right_nulls,
                        work_mem,
                    ))
                }
            };
            let joined = if let Some(using) = resolved_using.as_ref() {
                shape_join_using_output(joined, *kind, &left_schema, &right_schema, using)
            } else {
                Ok(joined)
            }?;
            alias_join_operator(joined, alias.as_deref(), column_aliases)
        }
        _ => unreachable!("join source builder called for a different source kind"),
    }
}
