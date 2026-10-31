//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind a correlated join against its producer's complete output descriptor.

use super::{
    alias_join_operator, bind_join_on_schema, bind_source_plan_schema,
    build_join_operator_with_ctes, combine_filters, join_using_predicate, null_row_for_schema,
    physical_exec_error, qualifier_filter, require_lateral_function_execute, resolve_join_using,
    shape_join_using_output, unproduced_column, CteScope, PhysicalOperator, QualifierFilters,
    QueryLateralSource, RowSchema, RowSchemaExecution, SQLError, SQLParam, SourceContext,
    SourcePlan,
};
use crate::physical::construction::OperatorBuild;
use crate::{OwnedPhysicalRow, PhysicalRow, SQLRowSource, SQLRowStream};
use uqa_sql::semantics::from_qualifier_set;

#[cfg(test)]
mod tests;

pub(super) struct LateralBinding<'a> {
    pub(super) join: &'a SourcePlan,
    pub(super) right: &'a SourcePlan,
    pub(super) input_schema: RowSchema,
    pub(super) filters: Option<&'a QualifierFilters>,
    pub(super) recheck_path: Option<&'a [u8]>,
}

#[expect(
    clippy::too_many_lines,
    reason = "binds one correlated source and its join predicates"
)]
pub(super) fn build_lateral_join<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    left: Box<dyn PhysicalOperator + 'a>,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    binding: LateralBinding<'_>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let SourcePlan::Join {
        kind,
        on,
        using,
        natural,
        alias,
        column_aliases,
        ..
    } = binding.join
    else {
        unreachable!("lateral binding requires a join");
    };
    OperatorBuild::build(left, |left| {
        let input_schema = binding.input_schema;
        let mut bound = bind_source_plan_schema(
            context.ctes.routines,
            binding.right,
            params,
            ctes,
            Some(&input_schema),
        )?;
        require_lateral_function_execute(context, binding.right, &input_schema, params, ctes)?;
        let pinned = binding
            .recheck_path
            .and_then(|path| ctes.recheck_source_row(path));
        if let Some(source) = &pinned {
            if bound.columns_are_open(None) {
                bound = source.schema.clone();
            }
        }
        let specialized = if pinned.is_none() {
            super::output_filter::specialize_lateral_source(
                context,
                binding.right,
                binding.filters,
            )?
        } else {
            None
        };
        let filters = if specialized.is_some() {
            None
        } else {
            binding.filters
        };
        let right = specialized.as_deref().unwrap_or(binding.right);
        let runtime_columns = bound.columns_are_open(None);
        let bind = |left_schema: RowSchema, right_schema: RowSchema| {
            let left_nulls = null_row_for_schema(left_schema.columns());
            let right_nulls = null_row_for_schema(right_schema.columns());
            let (on, evaluator) = bind_join_on_schema(
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
                .or(on);
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
            let pinned_right = pinned
                .as_ref()
                .map(|source| {
                    source
                        .schema
                        .relayout_physical_row(source.row.clone(), &right_schema)
                        .map(|row| OwnedPhysicalRow::new(right_schema.clone(), row))
                        .map_err(physical_exec_error)
                })
                .transpose()?;
            let qualifiers = from_qualifier_set(right);
            let right_filter = combine_filters(
                qualifiers
                    .iter()
                    .filter_map(|qualifier| qualifier_filter(filters, qualifier)),
            )
            .map(|predicate| -> Result<_, SQLError> {
                if let Some(column) = qualifiers
                    .iter()
                    .find_map(|qualifier| unproduced_column(&predicate, qualifier, &right_schema))
                {
                    return Ok(Err(column));
                }
                let evaluator = context.relational.evaluator(params, ctes)?;
                let predicate = evaluator.bind_type_introspection(predicate, &right_schema);
                Ok(Ok((
                    crate::scalar::PreparedExpressions::scalar(predicate),
                    evaluator,
                )))
            })
            .transpose()?;
            let source = QueryLateralSource {
                context: *context,
                right: right.clone(),
                right_filter,
                on: effective_on.map(|expression| {
                    let joined = RowSchema::join(
                        &left_schema,
                        &right_schema,
                        left_nulls.keys().chain(right_nulls.keys()).cloned(),
                    );
                    (
                        crate::scalar::PreparedExpressions::scalar(
                            evaluator.bind_type_introspection(expression, &joined),
                        ),
                        evaluator.clone(),
                    )
                }),
                function_arguments: match right {
                    SourcePlan::Function { args, .. } => {
                        let evaluator = context.relational.evaluator(params, ctes)?;
                        Some(crate::scalar::PreparedExpressions::scalars(
                            args.iter()
                                .map(|argument| {
                                    evaluator
                                        .bind_type_introspection(argument.clone(), &input_schema)
                                })
                                .collect(),
                        ))
                    }
                    _ => None,
                },
                routine_descriptions:
                    uqa_sql::routines::invocation::descriptions::RoutineCallDescriptions::default(),
                expression_runtime: std::sync::OnceLock::new(),
                function_group: super::function_source::PreparedFunctionGroup::default(),
                params,
                ctes: ctes.copy_with_control(&context.ctes.production_resources()?.control())?,
                input_schema: input_schema.clone(),
                right_schema: right_schema.clone(),
                pinned_right,
                prepared_right: None,
                runtime_columns,
            };
            Ok((
                source,
                left_schema,
                right_schema,
                left_nulls,
                right_nulls,
                resolved_using,
            ))
        };
        left.attach(|left| {
            let (
                left,
                first,
                (mut source, left_schema, right_schema, left_nulls, right_nulls, using),
            ) = if bound.columns_are_open(None) {
                let (left, first) = describe_from_first_row(left, |left_row| {
                    let mut scope =
                        ctes.copy_with_control(&context.ctes.production_resources()?.control())?;
                    let row = match ctes.row_lock_outer_row() {
                        Some(outer) => OwnedPhysicalRow::new(
                            input_schema.clone(),
                            PhysicalRow::concat_left_owned(left_row.row.clone(), &outer.row),
                        ),
                        None => left_row.clone(),
                    };
                    scope.set_row_lock_outer_row(row);
                    build_join_operator_with_ctes(context, right, params, &mut scope, None, None)
                })?;
                let (left, first, binding) = OperatorBuild::pair(left, first, |left, first| {
                    bind(left.row_schema().clone(), first.row_schema().clone())
                })?;
                (left, Some(first), binding)
            } else {
                let (left, binding) =
                    OperatorBuild::run(left, |left| bind(left.row_schema().clone(), bound))?;
                (left, None, binding)
            };
            source.prepared_right = first;
            let join = Box::new(crate::LateralJoin::new_with_right_schema(
                left,
                Box::new(source),
                *kind,
                left_nulls,
                right_nulls,
                right_schema.clone(),
            ));
            OperatorBuild::build(join, |join| {
                if let Some(using) = using.as_ref() {
                    join.attach(|input| {
                        shape_join_using_output(input, *kind, &left_schema, &right_schema, using)
                    })?;
                }
                join.attach(|input| alias_join_operator(input, alias.as_deref(), column_aliases))
            })
        })
    })
}

type Input<'a> = Box<dyn PhysicalOperator + 'a>;

/// Runtime-described callbacks need their first actual invocation before
/// consumers bind names and wildcards. Retain that invocation and replay its
/// one outer row; neither input is evaluated twice or drained for metadata.
fn describe_from_first_row<'a>(
    left: Input<'a>,
    describe: impl FnOnce(&OwnedPhysicalRow) -> Result<Input<'a>, SQLError>,
) -> Result<(Input<'a>, Input<'a>), SQLError> {
    let schema = left.row_schema().clone();
    let mut rows = SQLRowStream::from_source(
        crate::physical::demand::OperatorRows::open(left).map_err(physical_exec_error)?,
    );
    let first = rows.next().transpose()?.ok_or_else(|| SQLError::Routine {
        sqlstate: "42P18".into(),
        message: "an empty correlated host source requires registered output columns".into(),
    })?;
    let right = describe(&first).map_err(|error| rows.fail(error))?;
    let rows = SQLRowStream::from_source(PreparedLeft {
        first: Some(first.row),
        rows,
    });
    Ok((
        Box::new(crate::PhysicalRowIteratorScan::from_stream(schema, rows)),
        right,
    ))
}

struct PreparedLeft<'a> {
    first: Option<PhysicalRow>,
    rows: SQLRowStream<'a, OwnedPhysicalRow>,
}

impl SQLRowSource<PhysicalRow> for PreparedLeft<'_> {
    fn next_row(&mut self) -> Result<Option<PhysicalRow>, SQLError> {
        if let Some(first) = self.first.take() {
            return Ok(Some(first));
        }
        self.rows
            .next()
            .transpose()
            .map(|row| row.map(|row| row.row))
    }
    fn close(&mut self) -> Result<(), SQLError> {
        self.first = None;
        self.rows.close()
    }
}
