//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection, ordering, filtering, and relational physical-operator assembly.

use super::{ScalarExpr, Value};

#[derive(Default)]
pub(in crate::sql) struct RelationalResjunk {
    distinct_on: Vec<(usize, uqa_sql::ast::InternalColumnRef)>,
    order_by: Vec<(usize, uqa_sql::ast::InternalColumnRef)>,
}

impl RelationalResjunk {
    pub(in crate::sql) fn columns(&self) -> Vec<uqa_sql::ast::InternalColumnRef> {
        self.distinct_on
            .iter()
            .chain(&self.order_by)
            .map(|(_, column)| *column)
            .collect()
    }

    pub(in crate::sql) fn is_empty(&self) -> bool {
        self.distinct_on.is_empty() && self.order_by.is_empty()
    }
}

mod aggregation;
mod limit;
mod operators;
mod ordering;
mod output;
mod projection;
mod row_at_a_time;
mod row_locking;

pub(in crate::sql) use limit::attach_order_limit;
pub(in crate::sql) use operators::build_relational_operator;
pub(in crate::sql) use ordering::{
    identity_order_columns, order_projection, resolve_order_expression,
};
pub(in crate::sql) use output::{
    execute_filter_physical_rows, execute_query_block_operator_output,
};
pub(in crate::sql) use projection::{
    close_after_physical_failure, expand_bound_projection_stars, expand_from_star_columns,
    physical_exec_error, physical_projections, physical_work_mem_bytes,
    user_function_output_columns, visible_projection_source_position,
};
pub(in crate::sql) use row_locking::build_row_lock_recheck_operator;

pub(in crate::sql) fn expr_contains_jsonpath_fts_match(expr: &ScalarExpr) -> bool {
    let mut found = false;
    expr.visit(&mut |part| {
        if expr_is_jsonpath_fts_match(part) {
            found = true;
        }
    });
    found
}

pub(in crate::sql) fn expr_is_jsonpath_fts_match(expr: &ScalarExpr) -> bool {
    matches!(
        expr,
        ScalarExpr::Func { name, args, .. }
            if name.eq_ignore_ascii_case("fts_match")
                && matches!(
                    args.get(1),
                    Some(ScalarExpr::Literal(Value::Str(path))) if path.trim_start().starts_with('$')
                )
    )
}

#[cfg(test)]
mod tests;
