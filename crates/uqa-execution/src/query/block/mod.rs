//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical SELECT query blocks over local, foreign, and composed sources.

pub mod context;
mod execution;
mod facets;
pub mod foreign;
mod table;
pub mod where_filter;
use crate::query::privileges::ensure_select_privileges_for_query_block;
use crate::query::{
    binding::{
        bind_source_plan_schema, bind_source_plan_schema_for_execution, binding_context,
        validate_query_block_expression_types, validate_query_block_references,
    },
    consumer::QueryOutputMode,
    output::{QueryOutput, QueryRows},
    projection::{expand_from_star_columns, physical_work_mem_bytes},
    relational::output::execute_query_block_operator_output,
    scored_input::{ScoredDocumentSource, ScoredInput},
    sources::SourceContext,
    CteScope,
};
use crate::{eval_scalar, ScalarEvalContext};
use facets::{
    build_facet_output, facet_projection_fields, post_retrieval_score_top_k,
    score_limited_text_filter, score_order_top_k, FacetExecution,
};
use foreign::run_single_foreign_select_output;
use table::run_single_table_select_output;
use uqa_core::{ScoredEntry, Value};
use uqa_sql::{
    ast::BinaryOp,
    binding::{overlay_outer_schema, with_query_table_pseudo_columns},
    plan::{
        source_projection::SourceProjection, AccessPathPlan, ComputePlan, ProjectionPlan,
        QueryBlockPlan, SourcePlan,
    },
    semantics::sets::validation::{
        validate_query_set_contexts, validate_source_set_contexts_before_build,
    },
    semantics::{
        expect_column_name, flatten_and_filter_parts, projection_columns,
        source_filters::combine_filters as combine_filter_parts, SCORE_COLUMN, TABLE_OID_COLUMN,
    },
    SQLError, SQLParam, ScalarExpr,
};
use where_filter::execute_mixed_where;

pub struct SingleRelation<'a> {
    pub reference_name: &'a str,
    pub relation_name: &'a str,
    pub qualifier: &'a str,
}

pub use execution::execute_query_block_output;

fn run_select_without_from_output<'a, S: Clone + 'static>(
    context: &SourceContext<'a, S>,
    original: &'a QueryBlockPlan,
    statement: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope<S>,
    output: QueryOutputMode<'a>,
) -> Result<QueryOutput, SQLError> {
    let columns = projection_columns(&statement.projections);
    let operator = Box::new(crate::TableScan::from_physical_rows(
        crate::RowSchema::default(),
        vec![crate::PhysicalRow::default()],
    ));
    execute_query_block_operator_output(
        context.relational,
        operator,
        statement.r#where.clone(),
        statement,
        original,
        params,
        ctes,
        columns,
        output,
    )
}

fn expr_contains_jsonpath_fts_match(expr: &ScalarExpr) -> bool {
    let mut found = false;
    expr.visit(&mut |part| {
        if expr_is_jsonpath_fts_match(part) {
            found = true;
        }
    });
    found
}
fn expr_is_jsonpath_fts_match(expr: &ScalarExpr) -> bool {
    matches!(expr, ScalarExpr::Func { name, args, .. } if name.eq_ignore_ascii_case("fts_match")
        && matches!(args.get(1), Some(ScalarExpr::Literal(Value::Str(path))) if path.trim_start().starts_with('$')))
}
