//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pull-based assembly of tables, joins, values, functions, CTEs, and derived queries.

pub mod context;
use crate::physical::physical_exec_error;
use crate::query::{
    binding::{bind_source_plan_schema, bind_source_plan_schema_for_execution},
    consumer::QueryOutputMode,
    locking::apply_propagated_view_lock,
    output::{query_output_shared, QueryOutput},
    projection::physical_work_mem_bytes,
    source_projection::{alias_join_operator, qualify_source_operator_with_columns},
    table_functions::{rows_from::RowsFromOperator, TableFunctionCall},
    CteScope,
};
use crate::scalar::plan::PlanSubqueryArena;
use crate::{eval_scalar, PhysicalOperator, RowSchema, RowSchemaExecution, ScalarEvalContext};
pub use context::SourceContext;
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_sql::{
    ast::JoinKind,
    plan::{
        source_projection::{ColumnPrune, QualifierFilters},
        AccessPathPlan, ComputePlan, JoinExecutionStrategy, QueryPlan, RelationalPlan, SourcePlan,
    },
    semantics::{
        cte_names::query_cte_names,
        join_predicates::{decide_join_sides, join_conjuncts},
        resolve_user_table_function,
        source_filters::{combine_filters, qualifier_filter, qualifier_for},
        table_function_column_types, validate_table_function_alias_count,
        validate_table_function_column_definition, TableFunctionTypeRequest,
    },
    SQLError, SQLParam, ScalarExpr,
};

mod function_source;
mod join_source;
mod join_validation;
mod lateral_source;
mod source_operator;
mod source_qualification;
mod streaming_subquery;
mod subquery_source;
mod table_source;
mod values_source;
use function_source::{build_function_group_source_operator, build_function_source_operator};
use join_source::build_join_source_operator;
use join_validation::validate_join_on_schema;
use lateral_source::QueryLateralSource;
use source_operator::build_join_operator_with_ctes_at_path;
use source_qualification::shape_join_using_output;
use source_qualification::{
    attach_qualifier_filter, execute_view_plan_output_with_parent_cache, null_row_for_schema,
};
use streaming_subquery::try_build_streaming_subquery_operator;
use subquery_source::build_subquery_source_operator;
use table_source::build_table_source_operator;
use uqa_sql::semantics::{join_using_predicate, resolve_join_using};
use values_source::build_values_source_operator;

pub use source_operator::{build_join_operator_with_ctes, build_join_operator_with_recheck_pins};

pub fn build_join_spill_with_ctes<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope<S>,
) -> Result<crate::SharedSpill, SQLError> {
    let mut bound = source.clone();
    bind_source_plan_schema_for_execution(context.ctes.routines, &mut bound, params, ctes, None)?;
    let operator = build_join_operator_with_ctes(context, &bound, params, ctes, None, None)?;
    let columns = operator.schema().to_vec();
    let output = crate::query::collection::collect_query_operator(
        context.relational.runtime,
        columns,
        operator,
        QueryOutputMode::SharedSpill,
    )?;
    query_output_shared(output, "DML FROM")
}

pub mod recheck;

pub mod lateral_query;
