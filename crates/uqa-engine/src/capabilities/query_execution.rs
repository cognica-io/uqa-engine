//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind statement execution capabilities to the selected session and catalog generation.
use crate::{session::StatementReadSnapshot, sql::CteScope, Engine};
use std::collections::BTreeMap;
use uqa_execution::query::statement::context::{
    CteFilterPlanning, DirectionalQueryFactory, ScopedStatementOperation, StatementContext,
    StatementSnapshots,
};
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam, ScalarExpr};
impl Engine {
    pub(crate) fn statement_execution_context(
        &self,
    ) -> StatementContext<'_, StatementReadSnapshot> {
        StatementContext {
            source: self.source_execution_context(),
            mutation: self.mutation_execution_context(),
            snapshots: self,
            directional: self,
            cte_filters: self,
        }
    }
}
impl StatementSnapshots<StatementReadSnapshot> for Engine {
    fn capture(&self) -> Result<StatementReadSnapshot, SQLError> {
        self.capture_statement_read_snapshot()
    }
    fn with_snapshot(
        &self,
        snapshot: &StatementReadSnapshot,
        operation: &mut dyn ScopedStatementOperation<StatementReadSnapshot>,
    ) -> Result<(), SQLError> {
        let selected = self.statement_read_snapshot_engine(snapshot);
        operation.run(&selected.statement_execution_context())
    }
}
impl CteFilterPlanning<StatementReadSnapshot> for Engine {
    fn output_filters(
        &self,
        plan: &QueryPlan,
        scope: &CteScope,
    ) -> Result<BTreeMap<String, (String, ScalarExpr)>, SQLError> {
        super::query_planning::cte_output_filters(self, plan, scope)
    }
}
impl DirectionalQueryFactory<StatementReadSnapshot> for Engine {
    fn query_operator(
        &self,
        plan: QueryPlan,
        params: Vec<SQLParam>,
        scope: CteScope,
        schema: uqa_execution::RowSchema,
    ) -> Result<Box<dyn uqa_execution::PhysicalOperator>, SQLError> {
        self.directional_query_operator(plan, params, scope, schema)
    }
}
