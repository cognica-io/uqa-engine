//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind statement execution capabilities to the selected session and catalog generation.
use crate::{session::StatementReadSnapshot, sql::CteScope, Engine};
use std::collections::BTreeMap;
use uqa_execution::query::statement::context::{
    CteFilterPlanning, DirectionalQueryFactory, QueryContext, QuerySnapshots, ScopedQueryOperation,
    SnapshotSource,
};
use uqa_sql::{plan::QueryPlan, SQLError, SQLParam, ScalarExpr};
impl Engine {
    pub(crate) fn query_execution_context(&self) -> QueryContext<'_, StatementReadSnapshot> {
        QueryContext {
            generation: None,
            source: self.source_execution_context(),
            snapshots: self,
            directional: self,
            cte_filters: self,
        }
    }
}
impl QuerySnapshots<StatementReadSnapshot> for Engine {
    fn with_snapshot(
        &self,
        snapshot: &StatementReadSnapshot,
        operation: &mut dyn ScopedQueryOperation<StatementReadSnapshot>,
    ) -> Result<(), SQLError> {
        let selected = self.statement_read_snapshot_engine(snapshot);
        let mut context = selected.query_execution_context();
        context.generation = Some(snapshot);
        operation.run(&context)
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

impl SnapshotSource<StatementReadSnapshot> for Engine {
    fn capture(&self) -> Result<StatementReadSnapshot, SQLError> {
        self.capture_statement_read_snapshot()
    }
}
