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

struct SessionQueryInputs {
    engine: Engine,
    plan: OwnedQueryPlan,
    params: Produced<Vec<SQLParam>>,
    _memory: Option<MemoryReservation>,
}

struct RetainedSessionQuery {
    inputs: std::sync::Arc<SessionQueryInputs>,
    scope: CteScope<StatementReadSnapshot>,
    _memory: Option<MemoryReservation>,
}

struct RecursiveSessionQuery(RetainedSessionQuery);

impl RetainedQueryInput for RetainedSessionQuery {
    fn build(&self) -> Result<Option<Box<dyn PhysicalOperator + '_>>, SQLError> {
        try_build_retained_query_operator(
            &self.inputs.engine.query_execution_context(),
            &self.inputs.plan,
            &self.inputs.params,
            &self.scope,
        )
    }
}

impl RetainedQueryInput for RecursiveSessionQuery {
    fn build(&self) -> Result<Option<Box<dyn PhysicalOperator + '_>>, SQLError> {
        let session = &self.0;
        uqa_execution::query::statement::retained::build_recursive_term(
            &session.inputs.engine.query_execution_context(),
            &session.inputs.plan,
            &session.inputs.params,
            &session.scope,
        )
        .map(Some)
    }
}

impl uqa_execution::query::cte::context::RecursiveTerm for RetainedSessionQuery {
    fn scan(
        &self,
        working: Option<(&str, uqa_execution::SharedSpill, Option<usize>)>,
    ) -> Result<Box<dyn PhysicalOperator>, SQLError> {
        let resources = self
            .inputs
            .engine
            .cte_execution_context()
            .production_resources()?;
        let control = resources.control();
        let mut scope = self.scope.copy_with_control(&control)?;
        if let Some((name, rows, width)) = working {
            scope = scope.with_recursive_cte_with_control(name, rows, width, &control)?;
        }
        let input = Self {
            inputs: self.inputs.clone(),
            scope,
            _memory: control.reserve(size_of::<Self>())?,
        };
        RetainedQueryOperator::try_new_with_control(
            Box::new(RecursiveSessionQuery(input)),
            &control,
        )?
        .expect("a bound recursive term always has an execution path")
        .into_boxed(&control)
        .map(|operator| operator as Box<dyn PhysicalOperator>)
    }

    fn functions(&self) -> &dyn uqa_sql::expr::EngineHook {
        &self.inputs.engine
    }
}

impl Engine {
    fn capture_query_input(
        &self,
        plan: &QueryPlan,
        params: &[SQLParam],
        scope: &CteScope<StatementReadSnapshot>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<RetainedSessionQuery, SQLError> {
        let plan = OwnedQueryPlan::from_produced(plan.clone_with_control(control)?, control)?;
        let mut retained_params = ProductionVec::new(*control);
        retained_params.reserve(params.len())?;
        for parameter in params {
            retained_params.push_produced(parameter.clone_with_control(control)?)?;
        }
        let params = retained_params.finish()?;
        let scope = scope.copy_with_control(control)?;
        let memory = control.reserve(size_of::<SessionQueryInputs>())?;
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
        Ok(RetainedSessionQuery {
            inputs: std::sync::Arc::new(SessionQueryInputs {
                engine,
                plan,
                params,
                _memory: memory,
            }),
            scope,
            _memory: control.reserve(size_of::<RetainedSessionQuery>())?,
        })
    }

    pub(crate) fn retain_recursive_query(
        &self,
        plan: &QueryPlan,
        params: &[SQLParam],
        scope: &CteScope<StatementReadSnapshot>,
    ) -> Result<Box<dyn uqa_execution::query::cte::context::RecursiveTerm>, SQLError> {
        let resources = self.cte_execution_context().production_resources()?;
        Ok(Box::new(self.capture_query_input(
            plan,
            params,
            scope,
            &resources.control(),
        )?))
    }

    pub(crate) fn retained_query_operator(
        &self,
        plan: &QueryPlan,
        params: &[SQLParam],
        scope: &CteScope<StatementReadSnapshot>,
    ) -> Result<Option<Box<dyn PhysicalOperator>>, SQLError> {
        let resources = self.cte_execution_context().production_resources()?;
        let control = resources.control();
        let input = self.capture_query_input(plan, params, scope, &control)?;
        RetainedQueryOperator::try_new_with_control(Box::new(input), &control)?
            .map(|operator| {
                operator
                    .into_boxed(&control)
                    .map(|operator| operator as Box<dyn PhysicalOperator>)
            })
            .transpose()
    }
}
