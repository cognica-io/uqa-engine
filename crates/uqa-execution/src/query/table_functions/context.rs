//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose table-function evaluation from live runtime, catalog, routine, and retrieval services.

use crate::operator_tree::runtime::TreeExecutionContext;
use crate::query::{cypher::CypherTableRuntime, runtime::QueryRuntimeView};
use crate::routines::invocation::context::RoutineInvocationContext;
use crate::scalar::plan::PhysicalSubqueryRunner;
use uqa_core::Value;
use uqa_operators::OperatorTree;
use uqa_sql::{
    ast::OperatorJoinRelations, expr::EngineHook, plan::QueryPlan,
    semantics::graph_functions::GraphNameCatalog, SQLError, SQLParam, ScalarExpr,
};
use uqa_storage::FtsIndexStat;

pub trait TableFunctionSession {
    fn listening_channels(&self) -> Vec<String>;
    fn sequence_data(&self, args: &[Value]) -> Result<Value, SQLError>;
    fn sequence_parameters(&self, args: &[Value]) -> Result<Value, SQLError>;
    /// The OID of the role with this name in the statement's catalog.
    fn role_oid(&self, name: &str) -> Result<Option<i64>, SQLError>;
}

pub trait AnalyzerTableFunctions: crate::query::scalar_projection::AnalyzerRevisions {
    fn register_named_analyzer(&self, name: &str, config: &str) -> Result<(), String>;
    fn drop_named_analyzer(&self, name: &str) -> Result<bool, String>;
    fn list_named_analyzers(&self) -> Result<Vec<String>, String>;
    fn set_table_field_analyzer(
        &self,
        table: &str,
        field: &str,
        analyzer: &str,
        phase: &str,
    ) -> Result<(), SQLError>;
    fn fts_index_stats(&self, table: Option<&str>) -> Result<Vec<FtsIndexStat>, SQLError>;
}

/// Bind both relation operands before execution schedules their independent physical plans.
pub trait OperatorJoinBinding {
    fn lower(
        &self,
        name: &str,
        relations: Option<&OperatorJoinRelations>,
        args: &[ScalarExpr],
        params: &[SQLParam],
    ) -> Result<(OperatorJoinRelations, OperatorTree), SQLError>;
}

pub struct TableFunctionContext<'a> {
    pub runtime: QueryRuntimeView<'a>,
    pub session: &'a dyn TableFunctionSession,
    pub analyzers: &'a dyn AnalyzerTableFunctions,
    pub graph_names: &'a dyn GraphNameCatalog,
    pub cypher: &'a dyn CypherTableRuntime,
    pub routines: RoutineInvocationContext<'a>,
    pub retrieval: TreeExecutionContext<'a>,
    pub joins: &'a dyn OperatorJoinBinding,
    pub params: &'a [SQLParam],
    pub eval_hook: &'a dyn EngineHook,
    /// Catalog types and routines for binding argument expressions before evaluation.
    pub resolver: &'a dyn uqa_sql::FunctionTypeResolver,
    pub subquery_runner: &'a dyn PhysicalSubqueryRunner,
    pub subqueries: &'a [QueryPlan],
}
