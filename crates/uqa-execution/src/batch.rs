//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema-bound, allocation-light physical rows and batches.
//!
//! Column names belong to [`RowSchema`], not to every row. A physical row is
//! made from shared value fragments. Joins concatenate fragment handles while
//! schemas remap `(qualifier, column)` identities to physical slots; neither
//! operation rebuilds a string-keyed map or clones the contained values.

use std::sync::Arc;

use smallvec::SmallVec;
use uqa_core::Value;
use uqa_sql::expr::RowLookup;
use uqa_sql::ResultRow;

use crate::physical::ExecResult;

mod batches;
mod materialization;
mod owned_row;
mod physical_row;
mod physical_row_view;
mod row_lock_origins;
mod schema_runtime;

pub use batches::Batch;
use materialization::RowMaterializer;
pub use owned_row::OwnedPhysicalRow;
use physical_row::RowFragment;
pub use physical_row::{PhysicalRow, RowProjectionValue};
pub use physical_row_view::PhysicalRowView;
use row_lock_origins::concat_lock_origins;
pub use row_lock_origins::RowLockOrigin;
pub use schema_runtime::RowSchemaExecution;
pub use uqa_sql::schema::{ColumnIdentity, RowSchema};
pub(crate) use uqa_sql::schema::{PhysicalLayout, ProjectedSlot};

#[cfg(test)]
mod tests;

/// Default rows-per-batch hint.
pub const DEFAULT_BATCH_SIZE: usize = 1024;

const NULL_SLOT: usize = usize::MAX;
/// Keep the optional row-lock lineage pointer inside the pre-lineage 64-bit row footprint while retaining seven allocation-free join/projection fragments.
const INLINE_ROW_FRAGMENTS: usize = 7;
static NULL_VALUE: Value = Value::Null;
