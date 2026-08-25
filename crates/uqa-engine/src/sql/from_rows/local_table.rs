//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Local table scans and recursive physical source assembly.

use super::{
    alias_join_operator, apply_table_function_aliases, attach_qualifier_filter,
    build_info_schema_rows, build_table_function_row_stream_with_row, build_values_physical_rows,
    combine_filters, decide_join_sides, execute_query_plan_output,
    execute_view_plan_output_with_parent_cache, has_filters_for_qualifier,
    is_score_provenance_column, join_conjuncts, join_using_predicate, null_row_for_schema,
    physical_work_mem_bytes, propagated_join_filters, push_output_filter_into_query_plan,
    qualifier_filter, qualifier_for, qualify_source_operator, qualify_source_operator_with_columns,
    query_contains_volatile_function, query_cte_names, query_output_shared, resolve_join_using,
    resolve_user_table_function, shape_join_using_output, table_function_column_types,
    table_function_empty_schema, validate_table_function_alias_count,
    validate_table_function_column_definition, ColumnPrune, CteScope, Engine,
    EngineExpressionEvaluator, EngineLateralSource, HierarchyScoredDocumentSource,
    JoinExecutionStrategy, JoinKind, QualifierFilters, QueryOutputMode, ResultRow, SQLError,
    SQLParam, ScalarExpr, ScopedEngineHook, ScoredDocumentSource, ScoredInput, SourceEvalContext,
    SourcePlan, TableFunctionCall, TableFunctionTypeRequest, Value,
    TABLE_FUNCTION_ORDINALITY_COLUMN, TABLE_OID_COLUMN,
};

use crate::sql::select::{
    alias_query_output_to_shared, apply_propagated_view_lock, bind_source_plan_schema,
    materialize_plan_ctes, resolve_row_locks,
};
use crate::sql::virtual_relation_schema;
use std::sync::Arc;
use uqa_planner::{AccessPathPlan, ComputePlan, RelationalPlan};

#[path = "local_table/command_scan.rs"]
mod command_scan;
#[path = "local_table/join_validation.rs"]
mod join_validation;

use join_validation::validate_join_on_schema;

type StreamingLocalTableScan<'a> = (Box<dyn uqa_execution::PhysicalOperator + 'a>, bool);
type SharedLockOrigin = (Arc<str>, Arc<str>);

pub(in crate::sql) struct EngineTableRowSource {
    table_name: String,
    table: std::sync::Arc<crate::TableState>,
    column_definitions: Vec<uqa_sql::ast::ColumnDef>,
    columns: Vec<String>,
    schema: Vec<String>,
    physical_schema: uqa_execution::RowSchema,
    table_oid: Option<Value>,
    predicate: Option<uqa_execution::ProjectedPredicate>,
    estimated_cardinality: u64,
    after: Option<uqa_core::DocId>,
    lock_origin: Option<SharedLockOrigin>,
    recheck_pins: Option<Arc<Vec<crate::sql::select::RecheckDoc>>>,
    recheck_cursor: usize,
    command_changes: Option<
        Arc<
            std::collections::BTreeMap<
                uqa_core::DocId,
                Option<uqa_storage::document_store::Document>,
            >,
        >,
    >,
    command_change_after: Option<uqa_core::DocId>,
    command_base_after: Option<uqa_core::DocId>,
    command_base_ids: std::collections::VecDeque<uqa_core::DocId>,
    command_base_exhausted: bool,
}

/// One logical inheritance scan over independently stored physical tables.
/// Each child source retains its own lock origin and command overlay while the
/// SQL-visible row type remains the selected ancestor's row type.
pub(in crate::sql) struct EngineHierarchyRowSource {
    sources: std::collections::VecDeque<EngineTableRowSource>,
    schema: Vec<String>,
    physical_schema: uqa_execution::RowSchema,
    estimated_cardinality: u64,
}

impl EngineHierarchyRowSource {
    fn new(sources: Vec<EngineTableRowSource>) -> Result<Self, SQLError> {
        let first = sources.first().ok_or_else(|| {
            SQLError::Internal("inheritance scan was built without a physical table".into())
        })?;
        let schema = first.schema.clone();
        let physical_schema = first.physical_schema.clone();
        let estimated_cardinality = sources
            .iter()
            .map(|source| source.estimated_cardinality)
            .sum();
        Ok(Self {
            sources: sources.into(),
            schema,
            physical_schema,
            estimated_cardinality,
        })
    }
}

impl uqa_execution::RowSource for EngineHierarchyRowSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn physical_schema(&self) -> Option<&uqa_execution::RowSchema> {
        Some(&self.physical_schema)
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        Some(self.estimated_cardinality)
    }

    fn next_row(&mut self) -> uqa_execution::ExecResult<Option<ResultRow>> {
        Ok(self.next_batch(1)?.pop())
    }

    fn next_batch(&mut self, max_rows: usize) -> uqa_execution::ExecResult<Vec<ResultRow>> {
        let rows = self.next_physical_batch(max_rows)?;
        Ok(rows
            .iter()
            .map(|row| self.physical_schema.view(row).to_result_row())
            .collect())
    }

    fn next_physical_batch(
        &mut self,
        max_rows: usize,
    ) -> uqa_execution::ExecResult<Vec<uqa_execution::PhysicalRow>> {
        let mut rows = Vec::with_capacity(max_rows);
        while rows.len() < max_rows {
            let Some(source) = self.sources.front_mut() else {
                break;
            };
            let mut batch = source.next_physical_rows_batch(max_rows - rows.len())?;
            if batch.is_empty() {
                self.sources.pop_front();
            } else {
                rows.append(&mut batch);
            }
        }
        Ok(rows)
    }
}

#[path = "local_table/function_source.rs"]
mod function_source;
#[path = "local_table/hierarchy_retrieval.rs"]
mod hierarchy_retrieval;
#[path = "local_table/join_source.rs"]
mod join_source;
#[path = "local_table/row_source.rs"]
mod row_source;
#[path = "local_table/rows_from.rs"]
mod rows_from;
#[path = "local_table/source_operator.rs"]
mod source_operator;
#[path = "local_table/streaming_scan.rs"]
mod streaming_scan;
#[path = "local_table/streaming_subquery.rs"]
mod streaming_subquery;
#[path = "local_table/subquery_source.rs"]
mod subquery_source;
#[path = "local_table/table_source.rs"]
mod table_source;
#[path = "local_table/values_source.rs"]
mod values_source;

use function_source::{build_function_group_source_operator, build_function_source_operator};
use hierarchy_retrieval::build_hierarchy_retrieval_operator;
use join_source::build_join_source_operator;
use row_source::table_lock_origin;
use rows_from::RowsFromOperator;
use source_operator::build_join_operator_with_ctes_at_path;
use streaming_scan::try_streaming_local_table_scan;
use streaming_subquery::try_build_streaming_subquery_operator;
use subquery_source::build_subquery_source_operator;
use table_source::build_table_source_operator;
use values_source::build_values_source_operator;

pub(in crate::sql) use source_operator::{
    build_join_operator_with_ctes, build_join_operator_with_recheck_pins,
};
