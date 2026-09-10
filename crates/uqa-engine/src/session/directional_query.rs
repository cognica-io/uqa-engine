//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session ownership for directional child-query workers.
use crate::{session::StatementReadSnapshot, Engine};
use std::rc::Rc;
use uqa_execution::{
    query::{
        consumer::QueryRowConsumer,
        statement::{
            consumer::QueryOutputMode,
            directional::{DirectionalQueryPlanOperator, DirectionalQueryTask},
            directional_support::query_plan_backward_scan_support,
        },
        CteScope,
    },
    PhysicalOperator, RowSchema,
};
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam};
struct DirectionalSessionQuery {
    engine: Engine,
    plan: QueryPlan,
    params: Vec<SQLParam>,
    scope: CteScope<StatementReadSnapshot>,
}
impl DirectionalQueryTask for DirectionalSessionQuery {
    fn execute(self: Box<Self>, consumer: Rc<dyn QueryRowConsumer>) -> Result<(), SQLError> {
        let Self {
            engine,
            plan,
            params,
            mut scope,
        } = *self;
        let _statement_gate = engine.runtime.statement_gate.delegate_to_current_thread();
        uqa_execution::query::statement::execute_query_plan_output(
            &engine.statement_execution_context(),
            &plan,
            &params,
            &mut scope,
            QueryOutputMode::physical_consumer(consumer),
        )
        .map(|_| ())
    }
}
impl Engine {
    pub(crate) fn directional_query_operator(
        &self,
        plan: QueryPlan,
        params: Vec<SQLParam>,
        scope: CteScope<StatementReadSnapshot>,
        schema: RowSchema,
    ) -> Result<Box<dyn PhysicalOperator>, SQLError> {
        let engine = self.fork_session_portal_worker_engine()?;
        let support = query_plan_backward_scan_support(&engine, &plan);
        Ok(Box::new(DirectionalQueryPlanOperator::new(
            Box::new(DirectionalSessionQuery {
                engine,
                plan,
                params,
                scope,
            }),
            support,
            schema,
        )))
    }
}
