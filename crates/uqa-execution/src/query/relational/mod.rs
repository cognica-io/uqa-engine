//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relational operator placement and execution boundaries.

pub mod context;
pub use context::{
    attach_lock_rows, build_set_projection, QueryExpressionFactory, RelationalContext,
    RowLockOperatorFactory,
};
pub mod aggregation;
pub mod filter;
pub mod limit;
pub mod operators;
pub mod ordering;
pub mod row_count;
pub use operators::build_relational_operator;

#[derive(Default)]
pub struct RelationalResjunk {
    pub distinct_on: Vec<(usize, uqa_sql::ast::InternalColumnRef)>,
    pub order_by: Vec<(usize, uqa_sql::ast::InternalColumnRef)>,
}

impl RelationalResjunk {
    pub fn columns(&self) -> Vec<uqa_sql::ast::InternalColumnRef> {
        self.distinct_on
            .iter()
            .chain(&self.order_by)
            .map(|(_, column)| *column)
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.distinct_on.is_empty() && self.order_by.is_empty()
    }
}

pub mod output;
pub mod project_row;
pub mod values;

pub mod sets;
