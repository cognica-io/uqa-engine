//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Combine repeatable query outputs through bounded set operations and ordering.

use super::{limit::attach_order_limit, RelationalContext};
use crate::query::{
    consumer::QueryOutputMode,
    ordering::identity_order_columns,
    output::QueryOutput,
    projection::{physical_exec_error, physical_work_mem_bytes},
    CteScope,
};
use uqa_sql::{ast::SetOpKind, plan::QueryBlockPlan, SQLError, SQLParam};

pub struct SetSpillExecution<'a> {
    kind: SetOpKind,
    all: bool,
    columns: Vec<String>,
    lhs: crate::SharedSpill,
    rhs: crate::SharedSpill,
    order_plan: Option<&'a QueryBlockPlan>,
    output_mode: QueryOutputMode<'a>,
}

impl<'a> SetSpillExecution<'a> {
    pub fn new(
        kind: SetOpKind,
        all: bool,
        columns: Vec<String>,
        lhs: crate::SharedSpill,
        rhs: crate::SharedSpill,
        order_plan: Option<&'a QueryBlockPlan>,
        output_mode: QueryOutputMode<'a>,
    ) -> Self {
        Self {
            kind,
            all,
            columns,
            lhs,
            rhs,
            order_plan,
            output_mode,
        }
    }
}

pub fn combine_set_spills_with_order_output<'a, S: Clone + 'static>(
    context: RelationalContext<'a, S>,
    execution: SetSpillExecution<'a>,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
) -> Result<QueryOutput, SQLError> {
    use crate::{ExternalSetOperation, PhysicalOperator};

    let public_positions = || {
        execution
            .columns
            .iter()
            .cloned()
            .enumerate()
            .map(|(position, column)| (column, position))
            .collect::<Vec<_>>()
    };
    let left: Box<dyn PhysicalOperator> = Box::new(crate::ColumnSelection::with_positions(
        Box::new(crate::SharedSpillScan::new(execution.lhs)),
        public_positions(),
    ));
    let right: Box<dyn PhysicalOperator> = Box::new(crate::ColumnSelection::with_positions(
        Box::new(crate::SharedSpillScan::new(execution.rhs)),
        public_positions(),
    ));
    let mut operation: Box<dyn PhysicalOperator + '_> = Box::new(
        ExternalSetOperation::new(
            left,
            right,
            execution.kind,
            execution.all,
            physical_work_mem_bytes(context.runtime)?,
        )
        .map_err(physical_exec_error)?,
    );
    if let Some(order_plan) = execution.order_plan {
        let output = identity_order_columns(&execution.columns);
        operation = attach_order_limit(
            operation,
            order_plan,
            &output,
            context,
            params,
            ctes,
            context.runtime,
            context.evaluator(params, ctes),
            None,
        )?;
    }
    crate::query::collection::collect_query_operator(
        context.runtime,
        execution.columns,
        operation,
        execution.output_mode,
    )
}
