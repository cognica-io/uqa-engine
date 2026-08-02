//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volcano-model physical operator pipeline.
//!
//! The canonical UQA behavior (`uqa/execution/*`) is built on top of Apache
//! Arrow `RecordBatch`es. This implementation keeps the same iterator
//! protocol (`open` / `next` / `close`) with row-oriented batches so the
//! engine can expose the operator surface without the `arrow-rs` build
//! dependency. The operator trait and operator catalogue defined here are
//! the execution contract.
//!
//! # Operator catalogue
//!
//! * [`scan::TableScan`] -- pulls every row of a logical relation into
//!   the pipeline. The relation source is supplied through
//!   [`scan::RowSource`], so the caller decides whether the rows come
//!   from the engine's per-table store, a CTE materialisation, or an
//!   FDW.
//! * [`relational::Filter`] -- keeps rows for which the predicate
//!   evaluates truthy.
//! * [`relational::Project`] -- emits a new schema by evaluating an
//!   expression list against each row.
//! * [`relational::Sort`] -- fully materialises the input, sorts by a
//!   list of `(expr, descending)` keys, and yields the sorted rows in
//!   batches.
//! * [`relational::Limit`] -- caps the row count at `offset + limit`,
//!   skipping the first `offset` rows.
//! * [`relational::HashAggregate`] -- group-by + aggregate over a
//!   blocking input, supporting `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`.
//! * [`relational::Window`] -- partition + order + frame-aware
//!   computation of `ROW_NUMBER` / `RANK` / `DENSE_RANK` / `LAG` /
//!   `LEAD` / `NTILE` and pure aggregate windows.
//! * [`spill::SpillBuffer`] -- disk-backed row buffer for blocking
//!   operators that exceed an in-memory budget.

#![allow(
    clippy::enum_glob_use,
    clippy::implicit_hasher,
    clippy::iter_without_into_iter,
    clippy::struct_field_names,
    clippy::single_match_else,
    clippy::option_if_let_else,
    clippy::map_unwrap_or,
    clippy::too_many_lines,
    clippy::filter_map_identity,
    clippy::needless_collect,
    clippy::explicit_iter_loop,
    clippy::manual_let_else,
    clippy::cast_lossless,
    clippy::explicit_auto_deref,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::similar_names,
    clippy::module_name_repetitions
)]

pub mod batch;
pub mod column_selection;
pub mod columnar_batch;
pub mod distinct;
pub mod external_sort;
pub mod join;
pub mod lateral_join;
pub mod map_rows;
pub mod physical;
pub mod project_set;
pub mod projected_predicate;
pub mod relational;
pub mod scalar;
pub mod scan;
pub mod set_operation;
pub mod spill;
pub mod spill_scan;

pub use batch::{Batch, RowSchema, DEFAULT_BATCH_SIZE};
pub use column_selection::ColumnSelection;
pub use columnar_batch::{ColumnVector, ColumnarBatch};
pub use distinct::{Distinct, ExactRowSet};
pub use external_sort::{ExternalSort, EXTERNAL_SORT_MERGE_FAN_IN};
pub use join::{HashJoin, NestedLoopJoin};
pub use lateral_join::{LateralJoin, LateralRows, LateralSource};
pub use map_rows::{MapRows, SharedRowMapper};
pub use physical::{
    ordering_satisfies, ExecError, ExecResult, OperatorBatchCursor, PhysicalOperator, PhysicalOrder,
};
pub use project_set::{ProjectRows, ProjectSet, SetProjector};
pub use projected_predicate::ProjectedPredicate;
pub use relational::{
    AggregateExecutor, AggregateKind, AggregateSpec, ExpressionEvaluator, Filter, HashAggregate,
    Limit, Project, RowPredicate, SetOperation, SharedExpressionEvaluator, SharedRowPredicate,
    Sort, SortKey, Window, WindowExecutor, WindowKind,
};
pub use scalar::{
    eval_call_arguments, eval_scalar, ScalarEvalContext, ScalarExpr, ScalarFrameBound, ScalarOrder,
    ScalarSubqueryRunner, ScalarWindowFrame, ScalarWindowSpec, SubqueryId, SubqueryResult,
};
pub use scan::{RowIteratorScan, RowSource, TableScan};
pub use set_operation::ExternalSetOperation;
pub use spill::{IndexedSpill, SharedSpill, SharedSpillReader, SpillBuffer};
pub use spill_scan::{SharedSpillScan, SpillScan};
