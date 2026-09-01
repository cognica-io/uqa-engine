//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volcano-model physical operator pipeline.
//!
//! The pipeline uses an `open` / `next` / `close` iterator protocol with
//! row-oriented batches so the
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
pub mod join_output;
pub mod lateral_join;
pub mod map_rows;
pub mod physical;
pub mod project_set;
pub mod projected_predicate;
pub mod projected_row;
pub mod relational;
pub mod scalar;
pub mod scan;
pub mod scope_overlay;
pub mod set_operation;
pub mod spill;
pub mod spill_scan;
pub mod type_resolution;

pub use batch::{
    Batch, ColumnIdentity, OwnedPhysicalRow, PhysicalRow, PhysicalRowView, RowLockOrigin,
    RowProjectionValue, RowSchema, DEFAULT_BATCH_SIZE,
};
pub use column_selection::ColumnSelection;
pub use columnar_batch::{ColumnVector, ColumnarBatch};
pub use distinct::{
    canonical_row_key, hash_canonical_row, try_pack_compact_text_pair, CanonicalRowHashSet,
    Distinct, ExactRowSet,
};
pub use external_sort::{ExternalSort, EXTERNAL_SORT_MERGE_FAN_IN};
pub use join::{HashJoin, NestedLoopJoin};
pub use join_output::{JoinOutput, JoinOutputSource};
pub use lateral_join::{LateralJoin, LateralRows, LateralSource};
pub use map_rows::{MapRows, SharedRowMapper};
pub use physical::{
    order_expression_position, ordering_satisfies, ExecError, ExecResult, OperatorBatchCursor,
    PhysicalOperator, PhysicalOrder,
};
pub use project_set::{
    PhysicalProjectRows, PhysicalProjectSet, PhysicalSetProjector, ProjectRows, ProjectSet,
    SetProjector,
};
pub use projected_predicate::ProjectedPredicate;
pub use projected_row::{ProjectedRow, ProjectedValueSlot};
pub use relational::{
    AggregateExecutor, AggregateKind, AggregateSpec, ExpressionEvaluator, Filter, HashAggregate,
    Limit, Project, ProjectionTarget, RowPredicate, SetOperation, SharedExpressionEvaluator,
    SharedRowPredicate, Sort, SortKey, Window, WindowExecutor, WindowKind,
};
pub use scalar::{
    eval_call_arguments, eval_scalar, scalar_call_argument, scalar_call_arguments,
    validate_scalar_call_arguments, ScalarCallArgument, ScalarEvalContext, ScalarExpr,
    ScalarFrameBound, ScalarOrder, ScalarSubqueryRunner, ScalarWindowFrame, ScalarWindowSpec,
    SubqueryId, SubqueryResult,
};
pub use scan::{PhysicalRowIteratorScan, RowIteratorScan, RowSource, TableScan};
pub use scope_overlay::ScopeOverlay;
pub use set_operation::ExternalSetOperation;
pub use spill::{IndexedSpill, SharedSpill, SharedSpillReader, SpillBuffer};
pub use spill_scan::{SharedSpillScan, SpillScan};
pub use type_resolution::{
    bind_type_introspection, bind_type_introspection_with_resolver,
    builtin_function_argument_targets, common_context_expression_type, common_type,
    equality_operand_type, foreign_key_operand_type, resolve_checksum_overload,
    resolve_gamma_overload, resolve_json_strip_overload, resolve_length_overload,
    resolve_md5_overload, resolve_reverse_overload, scalar_type, scalar_type_with_resolver,
    values_column_types, BuiltinFunctionOverload, FunctionTypeResolver, ResolvedChecksumOverload,
    ResolvedFunctionOverload, ResolvedGammaOverload, ResolvedJsonStripOverload,
    ResolvedLengthOverload, ResolvedMd5Overload, ResolvedReverseOverload,
    ResolvedStringBinaryOverload, ResolvedTextByteaOverload,
};
#[doc(hidden)]
pub use type_resolution::{
    builtin_binding_matches, builtin_name_matches, canonical_column_type_name,
    canonical_routine_type_name, effective_overload_argument_type,
    effective_overload_argument_type_with_params, fixed_builtin_return_type,
    function_call_argument_signature, function_resolution_error, is_fixed_builtin,
    match_builtin_function_overload, match_function_signature, match_routine_signature,
    rank_function_matches, resolve_fixed_builtin_call, resolve_local_builtin_overload,
    routine_polymorphic_type, routine_type_accepts_implicit_cast, routine_type_category,
    routine_type_is_preferred, FunctionCallArgumentSignature, FunctionParameterDescriptor,
    MatchedBuiltinFunction, MatchedFunctionSignature, MatchedRoutineSignature, RankedFunctionMatch,
    ResolvedFixedBuiltinCall, RoutineCallDescriptor, RoutineCoercionTarget,
    RoutineParameterDescriptor, RoutinePolymorphicFamily, RoutinePolymorphicType,
    RoutineSignatureMatchError, RoutineTypeSubstitutions, RoutineVariadicMode, RoutineVariadicPlan,
};
