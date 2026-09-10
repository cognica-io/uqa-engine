//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query execution state and physical plan construction.

pub mod scope;
pub use scope::CteScope;

pub mod binding;
pub mod runtime;

pub mod ordering;
pub mod projection;
pub mod row_at_a_time;

pub type PhysicalProjection = (crate::ProjectionTarget, crate::ScalarExpr);
pub type OutputColumnMapping = (String, crate::ScalarExpr);

pub mod routine_invocation;
pub mod set_projection;
pub mod table_functions;

pub mod expression;

pub mod recheck_source;
pub mod relational;

pub mod cursor;
pub mod output;

pub mod catalog_expression;

pub mod graph_effects;

pub mod source_projection;

pub mod locking;

pub mod document_projection;
pub mod generated;
pub mod local_table;
pub mod scored_input;
pub mod table_read;

pub mod collection;
pub mod consumer;

pub mod cte;

pub mod privileges;

pub mod table_sources;

pub mod sources;

pub mod block;

pub mod statement;
