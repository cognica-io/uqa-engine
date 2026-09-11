//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind table-function runtime services to retained Engine state and public command boundaries.

use crate::Engine;
use uqa_core::Value;
use uqa_execution::query::table_functions::context::{
    AnalyzerTableFunctions, OperatorJoinBinding, TableFunctionContext, TableFunctionSession,
};
use uqa_execution::scalar::plan::PhysicalSubqueryRunner;
use uqa_operators::OperatorTree;
use uqa_sql::semantics::graph_functions::GraphNameCatalog;
use uqa_sql::{
    ast::OperatorJoinRelations, expr::EngineHook, plan::QueryPlan, SQLError, SQLParam, ScalarExpr,
};
use uqa_storage::FtsIndexStat;

impl GraphNameCatalog for Engine {
    fn list_graphs(&self) -> Result<Vec<String>, SQLError> {
        self.list_graphs()
            .map_err(|err| SQLError::Internal(format!("read graph catalog: {err}")))
    }
}
impl TableFunctionSession for Engine {
    fn listening_channels(&self) -> Vec<String> {
        self.listening_channels()
    }
    fn sequence_data(&self, args: &[Value]) -> Result<Value, SQLError> {
        self.pg_get_sequence_data_value(args)
    }
    fn sequence_parameters(&self, args: &[Value]) -> Result<Value, SQLError> {
        self.pg_sequence_parameters_value(args)
    }
}
impl AnalyzerTableFunctions for Engine {
    fn register_named_analyzer(&self, name: &str, config: &str) -> Result<(), String> {
        self.register_named_analyzer(name, config)
    }
    fn drop_named_analyzer(&self, name: &str) -> Result<bool, String> {
        self.drop_named_analyzer(name)
    }
    fn list_named_analyzers(&self) -> Result<Vec<String>, String> {
        self.list_named_analyzers()
    }
    fn set_table_field_analyzer(
        &self,
        table: &str,
        field: &str,
        analyzer: &str,
        phase: &str,
    ) -> Result<(), String> {
        self.set_table_field_analyzer(table, field, analyzer, phase)
    }
    fn fts_index_stats(&self, table: Option<&str>) -> Result<Vec<FtsIndexStat>, SQLError> {
        self.fts_index_stats(table)
    }
}
impl OperatorJoinBinding for Engine {
    fn lower(
        &self,
        name: &str,
        relations: Option<&OperatorJoinRelations>,
        args: &[ScalarExpr],
        params: &[SQLParam],
    ) -> Result<(OperatorJoinRelations, OperatorTree), SQLError> {
        crate::operator_tree_bridge::lower_operator_join_table_function(
            self, name, relations, args, params,
        )
    }
}
impl Engine {
    pub(crate) fn table_function_context<'a>(
        &'a self,
        params: &'a [SQLParam],
        eval_hook: &'a dyn EngineHook,
        subquery_runner: &'a dyn PhysicalSubqueryRunner,
        subqueries: &'a [QueryPlan],
    ) -> TableFunctionContext<'a> {
        TableFunctionContext {
            runtime: self.query_runtime_view(),
            session: self,
            analyzers: self,
            graph_names: self,
            cypher: self,
            routines: self.routine_invocation_context(),
            retrieval: self.tree_execution_context(),
            joins: self,
            params,
            eval_hook,
            subquery_runner,
            subqueries,
        }
    }
}
