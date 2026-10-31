//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session ownership for directional child-query workers.
use crate::{session::StatementReadSnapshot, Engine};
use std::rc::Rc;
use uqa_core::memory::{MemoryReservation, Produced, ProductionVec};
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
use uqa_sql::{
    plan::{OwnedQueryPlan, QueryPlan},
    SQLError, SQLParam,
};
struct DirectionalSessionQuery {
    engine: Engine,
    plan: OwnedQueryPlan,
    params: Produced<Vec<SQLParam>>,
    scope: CteScope<StatementReadSnapshot>,
    _memory: Option<MemoryReservation>,
}
impl DirectionalQueryTask for DirectionalSessionQuery {
    fn execute(mut self: Box<Self>, consumer: Rc<dyn QueryRowConsumer>) -> Result<(), SQLError> {
        let _statement_gate = self
            .engine
            .runtime
            .statement_gate
            .delegate_to_current_thread();
        uqa_execution::query::statement::execute_query_plan_output(
            &self.engine.query_execution_context(),
            &self.plan,
            &self.params,
            &mut self.scope,
            QueryOutputMode::physical_consumer(consumer),
        )
        .map(|_| ())
    }
}
impl Engine {
    pub(crate) fn directional_query_operator(
        &self,
        plan: &QueryPlan,
        params: &[SQLParam],
        scope: &CteScope<StatementReadSnapshot>,
        schema: RowSchema,
    ) -> Result<Box<dyn PhysicalOperator>, SQLError> {
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
        let memory = control.reserve(size_of::<DirectionalSessionQuery>())?;
        let engine = self.fork_session_portal_worker_engine()?;
        let read_control = engine.query_retention_control()?;
        let support =
            query_plan_backward_scan_support(&engine, &engine, &plan, &params, &scope, control)?;
        Ok(DirectionalQueryPlanOperator::new_with_control(
            Box::new(DirectionalSessionQuery {
                engine,
                plan,
                params,
                scope,
                _memory: memory,
            }),
            support,
            schema,
            &control,
        )?
        .with_diagnostics(self.runtime.diagnostics.fork_with_control(read_control)))
    }
}
