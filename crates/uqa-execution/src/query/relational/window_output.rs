//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The output of a query block that calls window functions. The window results join each input row; with `ORDER BY`, the target list is appended to the row so the sort can still read source columns that the SELECT list leaves out, and the output columns are selected after sorting, as for an ordinary target list.

use super::aggregation::{append_distinct_set_projections, prepare_order_set_projections};
use super::limit::attach_order_limit;
use super::ordering::{
    attach_final_projection_order, attach_streaming_order_projection, order_projection,
    output_selection_positions, FinalProjectionExecution,
};
use super::{build_set_projection, RelationalContext, RelationalResjunk};
use crate::query::projection::{physical_projections, physical_work_mem_bytes};
use crate::query::CteScope;
use crate::window::{prepare_window_plan, PhysicalWindowExecutor, PreparedWindowPlan};
use crate::{ColumnSelection, PhysicalOperator, Project, RowSchema, Window};
use std::sync::Arc;
use uqa_sql::plan::QueryBlockPlan;
use uqa_sql::semantics::sets::validation::projections_may_return_set;
use uqa_sql::{FunctionTypeResolver, SQLError, SQLParam, ScalarExpr};

pub(super) fn attach_window_output<'a, S: Clone + 'static>(
    operator: Box<dyn PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    resjunk: &mut RelationalResjunk,
    type_resolver: &dyn FunctionTypeResolver,
    execution: FinalProjectionExecution<'a, '_, S>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let plan = prepare_window_plan(&statement.projections);
    let window = WindowOutput {
        work_mem_bytes: physical_work_mem_bytes(execution.runtime)?,
        schema: plan.output_schema(
            execution.context.catalog,
            operator.row_schema(),
            execution.params,
        )?,
        plan,
    };
    if statement.order_by.is_empty() {
        attach_unordered_output(operator, statement, resjunk, window, execution)
    } else {
        attach_ordered_output(
            operator,
            statement,
            resjunk,
            type_resolver,
            window,
            execution,
        )
    }
}

/// The prepared window calls of a query block and the rows they produce.
struct WindowOutput {
    plan: PreparedWindowPlan,
    schema: RowSchema,
    work_mem_bytes: usize,
}

/// Without `ORDER BY`, the target list replaces each row as the window results arrive.
fn attach_unordered_output<'a, S: Clone + 'static>(
    mut operator: Box<dyn PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    resjunk: &mut RelationalResjunk,
    window: WindowOutput,
    execution: FinalProjectionExecution<'a, '_, S>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let mut projections = physical_projections(window.plan.projections());
    let output_columns = order_projection(&statement.projections, operator.row_schema())?
        .1
        .into_iter()
        .enumerate()
        .map(|(position, (output, _))| (output, ScalarExpr::Position(position)))
        .collect::<Vec<_>>();
    resjunk.distinct_on.extend(append_distinct_set_projections(
        statement,
        &output_columns,
        &mut projections,
    )?);
    operator = window_operator(
        operator,
        window,
        (execution.context, execution.params, execution.ctes),
    );
    attach_final_projection_order(
        operator,
        (statement, &output_columns),
        projections,
        execution,
    )
}

/// With `ORDER BY`, the target list is appended to each row so the sort can read source columns the SELECT list leaves out, and the output columns are selected after sorting.
fn attach_ordered_output<'a, S: Clone + 'static>(
    mut operator: Box<dyn PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    resjunk: &mut RelationalResjunk,
    type_resolver: &dyn FunctionTypeResolver,
    window: WindowOutput,
    execution: FinalProjectionExecution<'a, '_, S>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let FinalProjectionExecution {
        context,
        params,
        ctes,
        runtime,
        evaluator,
    } = execution;
    let (mut physical, output) = order_projection(window.plan.projections(), &window.schema)?;
    let order_output = output.clone();
    resjunk.distinct_on.extend(append_distinct_set_projections(
        statement,
        &order_output,
        &mut physical,
    )?);
    let (order_statement, order_columns) = prepare_order_set_projections(
        context.catalog,
        type_resolver,
        statement,
        &order_output,
        &mut physical,
        &window.schema,
        params,
    )?;
    resjunk.order_by.extend(order_columns);
    let order_statement = order_statement.as_ref().unwrap_or(statement);
    operator = window_operator(operator, window, (context, params, ctes));
    if ctes.streams_command_progress() {
        operator = attach_streaming_order_projection(
            operator,
            order_statement,
            &order_output,
            physical,
            FinalProjectionExecution {
                context,
                params,
                ctes,
                runtime,
                evaluator,
            },
        )?;
    } else {
        operator = if projections_may_return_set(
            context.catalog,
            type_resolver,
            &physical,
            operator.row_schema(),
            params,
        )? {
            build_set_projection(
                operator,
                context,
                params,
                ctes,
                Arc::clone(&evaluator),
                crate::query::set_projection::SetProjectionOutput {
                    projections: physical,
                    pass_through: true,
                    batch_size: crate::DEFAULT_BATCH_SIZE,
                },
            )?
        } else {
            Box::new(Project::appending_target_evaluator(
                operator,
                physical,
                Arc::clone(&evaluator),
            ))
        };
        operator = attach_order_limit(
            operator,
            order_statement,
            &order_output,
            context,
            params,
            ctes,
            runtime,
            evaluator,
            None,
        )?;
    }
    let output = output_selection_positions(operator.row_schema(), output)?;
    Ok(Box::new(ColumnSelection::with_physical_positions(
        operator, output,
    )))
}

/// The window operator over `operator`'s rows, appending one column per window call.
fn window_operator<'a, S: Clone + 'static>(
    operator: Box<dyn PhysicalOperator + 'a>,
    window: WindowOutput,
    (context, params, ctes): (RelationalContext<'a, S>, &'a [SQLParam], &CteScope<S>),
) -> Box<dyn PhysicalOperator + 'a> {
    let source = operator.row_schema().clone();
    Box::new(Window::with_row_schema_executor(
        operator,
        window.schema,
        Box::new(PhysicalWindowExecutor::new(
            context.expression_scope(ctes.clone()),
            window.plan,
            params,
            source,
            window.work_mem_bytes,
        )),
    ))
}
