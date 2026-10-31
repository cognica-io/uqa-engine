//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical assembly for lateral, hash, and nested-loop joins.

/// Build the physical operator for a join source.
use super::{
    alias_join_operator, bind_join_on_schema, build_join_operator_with_ctes_at_path,
    physical_work_mem_bytes, shape_join_using_output, ColumnPrune, CteScope, JoinExecutionStrategy,
    PhysicalOperator, QualifierFilters, SQLError, SQLParam, SourceContext, SourcePlan,
};
use crate::physical::construction::OperatorBuild;
use crate::query::binding::sources::SourceBindings;
use crate::{HashJoin, NestedLoopJoin};
use uqa_sql::semantics::{join_using_predicate_owned, resolve_join_using_with_control};

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
    let resources = context.ctes.production_resources()?;
    let control = resources.control();
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
            let left_filters = filters
                .map(|filters| {
                    context
                        .planning
                        .propagated_join_filters(filters, right, left, on.as_ref())
                })
                .transpose()?
                .flatten();
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
            OperatorBuild::build(left_operator, |left_operator| {
                let source_binding = if ctes.source_outer_rewrites.is_some() {
                    let outer_schema = ctes.row_lock_outer_row().map(|row| row.schema.clone());
                    let preceding = uqa_sql::binding::SourceScope::new(outer_schema.as_ref())
                        .right_namespace(left, left_operator.row_schema(), *kind);
                    crate::query::binding::bind_outer_source_rewrites(
                        context.ctes.routines,
                        right,
                        params,
                        ctes,
                        uqa_sql::binding::SourceScope {
                            outer: outer_schema.as_ref(),
                            preceding: Some(&preceding),
                            inherited_lateral: *lateral,
                            retain_names: true,
                        },
                    )?
                } else {
                    None
                };
                let mut source_scope;
                let ctes = if let Some(binding) = &source_binding {
                    let arena_resources = context.ctes.production_resources()?;
                    source_scope = ctes.enter_scalar_subqueries_with_control(
                        &binding.subqueries,
                        &arena_resources.control(),
                    )?;
                    &mut *source_scope
                } else {
                    ctes
                };
                let bindings = bindings.filter(|_| source_binding.is_none());
                let right = source_binding
                    .as_ref()
                    .map_or(right, |binding| &binding.source);
                let implicit_lateral_function = match right.as_ref() {
                    SourcePlan::Function { name, .. } => {
                        let identity = name.to_ascii_lowercase();
                        let lower = uqa_sql::semantics::builtin_function_dispatch_name(&identity);
                        !uqa_sql::registry::is_operator_join_table_function(&lower)
                    }
                    SourcePlan::FunctionGroup { .. } => true,
                    _ => false,
                };
                let right_filters = filters
                    .map(|filters| {
                        context
                            .planning
                            .propagated_join_filters(filters, left, right, on.as_ref())
                    })
                    .transpose()?
                    .flatten();
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
                    let input_schema = lateral_input.expect("dependent source has a calling scope");
                    return left_operator.attach(|left| {
                        super::lateral_binding::build_lateral_join(
                            context,
                            left,
                            params,
                            ctes,
                            super::lateral_binding::LateralBinding {
                                join: from,
                                right,
                                input_schema,
                                filters: right_filter_ref,
                                recheck_path: right_recheck_path.as_deref(),
                            },
                        )
                    });
                }

                let right_operator = if right_empty {
                    super::empty_source_operator(
                        context,
                        right,
                        params,
                        ctes,
                        lateral_input.as_ref(),
                    )?
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
                let (
                    right_operator,
                    (
                        resolved_using,
                        effective_on,
                        evaluator,
                        hash_plan,
                        left_nulls,
                        right_nulls,
                        work_mem,
                    ),
                ) = OperatorBuild::run(right_operator, |_| {
                    let (on, evaluator) = bind_join_on_schema(
                        context,
                        on.as_ref(),
                        &left_schema,
                        &right_schema,
                        params,
                        ctes,
                    )?;
                    let resolved_using = resolve_join_using_with_control(
                        using.as_ref(),
                        *natural,
                        &left_schema,
                        &right_schema,
                        &control,
                    )?;
                    let effective_on = resolved_using
                        .as_ref()
                        .map(|using| {
                            join_using_predicate_owned(using, &left_schema, &right_schema, &control)
                        })
                        .transpose()?
                        .flatten()
                        .or(on);
                    let effective_on = effective_on
                        .map(|predicate| {
                            let schema = crate::RowSchema::join_with_control(
                                &left_schema,
                                &right_schema,
                                [],
                                &control,
                            )?;
                            evaluator.bind_type_introspection_owned(predicate, &schema, &control)
                        })
                        .transpose()?;
                    let hash_plan = super::join_keys::join_keys(
                        *kind,
                        effective_on.as_deref(),
                        &left_schema,
                        &right_schema,
                        params,
                        context.types,
                        context.volatility,
                        &control,
                    )?;
                    // Both descriptors already contain every NULL-extended attribute; no duplicate named NULL rows are needed for physical construction.
                    let left_nulls = uqa_sql::ResultRow::new();
                    let right_nulls = uqa_sql::ResultRow::new();
                    let effective_on = effective_on
                        .map(|predicate| predicate.finish(&control))
                        .transpose()?;
                    let work_mem = physical_work_mem_bytes(context.relational.runtime)?;
                    Ok((
                        resolved_using,
                        effective_on,
                        evaluator,
                        hash_plan,
                        left_nulls,
                        right_nulls,
                        work_mem,
                    ))
                })?;
                left_operator.attach(|left_operator| {
                    let joined: Box<dyn PhysicalOperator + 'a> = match (strategy, hash_plan) {
                        (JoinExecutionStrategy::Auto | JoinExecutionStrategy::Hash, Some(keys)) => {
                            drop(effective_on);
                            HashJoin::with_owned_expressions(
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
                                &control,
                            )?
                            .into_boxed(&control)?
                        }
                        (JoinExecutionStrategy::Auto | JoinExecutionStrategy::Hash, None) => {
                            NestedLoopJoin::with_owned_predicate(
                                left_operator,
                                right_operator,
                                *kind,
                                effective_on,
                                evaluator,
                                left_nulls,
                                right_nulls,
                                work_mem,
                                &control,
                            )?
                            .into_boxed(&control)?
                        }
                    };
                    Ok(joined)
                })?;
                if let Some(using) = resolved_using.as_ref() {
                    left_operator.attach(|input| {
                        shape_join_using_output(
                            input,
                            *kind,
                            &left_schema,
                            &right_schema,
                            using,
                            &control,
                        )
                    })?;
                }
                left_operator.attach(|input| {
                    alias_join_operator(input, alias.as_deref(), column_aliases, &control)
                })
            })
        }
        _ => unreachable!("join source builder called for a different source kind"),
    }
}
