//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Own the selected session and statement inputs while Execution retains a child pipeline.

use crate::{session::StatementReadSnapshot, Engine};
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
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam};

struct RetainedSessionQuery {
    engine: Engine,
    plan: QueryPlan,
    params: Vec<SQLParam>,
    scope: CteScope<StatementReadSnapshot>,
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
        plan: QueryPlan,
        params: Vec<SQLParam>,
        scope: CteScope<StatementReadSnapshot>,
    ) -> Result<Option<Box<dyn PhysicalOperator>>, SQLError> {
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
        RetainedQueryOperator::try_new(Box::new(RetainedSessionQuery {
            engine,
            plan,
            params,
            scope,
        }))
        .map(|operator| operator.map(|operator| Box::new(operator) as Box<dyn PhysicalOperator>))
    }
}
