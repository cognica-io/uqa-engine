//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL semantics shared by static analysis and runtime adapters.

pub mod aggregates;
mod ctes;
mod expression_shape;
mod functions;
mod projection;
mod source_shape;
pub use ctes::*;
pub use expression_shape::*;
pub use functions::*;
pub use projection::*;
pub use source_shape::*;

pub const DOC_ID_COLUMN: &str = "_doc_id";
pub const SCORE_COLUMN: &str = "_score";
pub const TABLE_OID_COLUMN: &str = "tableoid";
pub const XMIN_COLUMN: &str = "xmin";
pub const META_QUALIFIER: &str = "_meta";
pub const META_DOC_ID_COLUMN: &str = "doc_id";
pub const META_SCORE_COLUMN: &str = "score";
pub fn expr_contains_subquery(expr: &crate::ScalarExpr) -> bool {
    expr.contains_subquery()
}

mod bound_columns;
mod catalog_mutation;
mod join_using;
mod table_functions;
pub use bound_columns::*;
pub use catalog_mutation::*;
pub use join_using::*;
pub use table_functions::*;

pub mod volatility;

pub mod sets;

pub mod grouping_sets;

pub mod row_count;

pub mod windows;

pub mod effects;

pub mod partition;

pub mod source_filters;

pub mod join_predicates;

pub mod cte_names;

pub mod rules;

pub mod cte_validation;

pub mod locking;

pub fn doc_id_value(doc_id: uqa_core::DocId) -> Result<Value, SQLError> {
    i64::try_from(doc_id).map(Value::Int).map_err(|_| {
        SQLError::TypeMismatch(format!("document id {doc_id} exceeds the SQL BIGINT range"))
    })
}

use crate::SQLError;
use uqa_core::Value;

pub use projection::query_plan_output_columns;

pub use projection::{select_execution_stmt, should_defer_distinct_limit};

pub mod view_rewrite;

pub mod privileges;

pub mod view_privileges;

pub mod text_indexes;

pub mod mutation_qualifiers;

pub mod referential;

pub mod foreign_keys;
pub mod period;

pub mod conflict;

pub mod returning;

pub mod mutation_inputs;

pub mod constraint_catalog;

pub mod mutation_patch;

pub mod mutation_privileges;
