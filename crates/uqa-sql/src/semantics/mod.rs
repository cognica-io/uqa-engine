//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL semantics shared by static analysis and runtime adapters.

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
