//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Own the selected session and statement inputs while Execution retains a child pipeline.

use crate::{session::StatementReadSnapshot, Engine};
use uqa_core::memory::{MemoryReservation, Produced, ProductionVec};
use uqa_execution::{
    query::{
        statement::{
            retained::try_build_retained_query_operator,
            retained_owner::{RetainedQueryInput, RetainedQueryOperator},
        },
        CteScope,
    },
    PhysicalOperator,
};
use uqa_sql::{
    plan::{OwnedQueryPlan, QueryPlan},
    SQLError, SQLParam,
};

struct RetainedSessionQuery {
    engine: Engine,
    plan: OwnedQueryPlan,
    params: Produced<Vec<SQLParam>>,
    scope: CteScope<StatementReadSnapshot>,
    _memory: Option<MemoryReservation>,
}
impl RetainedQueryInput for RetainedSessionQuery {
    fn build(&self) -> Result<Option<Box<dyn PhysicalOperator + '_>>, SQLError> {
        try_build_retained_query_operator(
            &self.engine.query_execution_context(),
            &self.plan,
            &self.params,
            &self.scope,
        )
    }
}
impl Engine {
    pub(crate) fn retained_query_operator(
        &self,
        plan: &QueryPlan,
        params: &[SQLParam],
        scope: &CteScope<StatementReadSnapshot>,
    ) -> Result<Option<Box<dyn PhysicalOperator>>, SQLError> {
        let resources = self.cte_execution_context().production_resources()?;
        let control = resources.control();
        let plan = OwnedQueryPlan::from_produced(plan.clone_with_control(&control)?, &control)?;
        let mut retained_params = ProductionVec::new(control);
        retained_params.reserve(params.len())?;
        for parameter in params {
            retained_params.push_produced(parameter.clone_with_control(&control)?)?;
        }
        let params = retained_params.finish()?;
        let scope = scope.copy_with_control(&control)?;
        let memory = control.reserve(size_of::<RetainedSessionQuery>())?;
        let engine = if let Some(snapshot) = scope.command_cte_snapshot() {
            self.statement_read_snapshot_engine(&snapshot)
        } else if self.query_table_snapshots.is_some() {
            let mut retained = self.fork_session_portal_worker_engine()?;
            retained
                .query_transaction_overlay
                .clone_from(&self.query_transaction_overlay);
            retained
        } else {
            self.statement_read_snapshot_engine(&self.capture_statement_read_snapshot()?)
        };
        RetainedQueryOperator::try_new_with_control(
            Box::new(RetainedSessionQuery {
                engine,
                plan,
                params,
                scope,
                _memory: memory,
            }),
            &control,
        )?
        .map(|operator| {
            operator
                .into_boxed(&control)
                .map(|operator| operator as Box<dyn PhysicalOperator>)
        })
        .transpose()
    }
}
