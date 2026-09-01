//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Byte-bounded streaming physical `DISTINCT` operator.
//!
//! The operator keeps exact encoded keys in memory until their combined byte
//! size reaches `work_mem`. It then migrates every key to a temporary,
//! bucketed on-disk set. Disk probes compare the complete encoded key, so a
//! hash collision can never turn a new row into a duplicate. Output remains

mod encoding;
mod memory;
mod spill;

use std::path::{Path, PathBuf};

use crate::{
    Batch, ExecError, ExecResult, PhysicalOperator, RowSchema, ScalarExpr,
    SharedExpressionEvaluator,
};

use encoding::encode_key_borrowed;
pub use encoding::{canonical_row_key, hash_canonical_row, try_pack_compact_text_pair};
pub(crate) use encoding::{encode_key, encode_non_null_key, EncodedKey};
pub use memory::{CanonicalRowHashSet, ExactRowSet};
pub(crate) use spill::SeenKeySet;
#[cfg(test)]
use spill::{stable_hash, DISK_BUCKETS};

/// Default used by compatibility constructors. Engine callers should pass the
/// current session's `work_mem` through [`Distinct::all_with_work_mem`] or
/// [`Distinct::on_with_work_mem`].
pub const DEFAULT_DISTINCT_WORK_MEM_BYTES: usize = 64 * 1024 * 1024;

/// Stable SQL duplicate elimination.
///
/// With no key expressions, the complete positional output row is the key.
/// With expressions, the operator implements `DISTINCT ON`: it preserves the
/// first row for each evaluated key in child order.
pub struct Distinct<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    keys: Option<Vec<ScalarExpr>>,
    evaluator: Option<SharedExpressionEvaluator<'a>>,
    schema: RowSchema,
    work_mem_bytes: usize,
    spill_directory: Option<PathBuf>,
    seen: SeenKeySet,
}

impl<'a> Distinct<'a> {
    /// Construct a bounded full-row `DISTINCT` with the compatibility default
    /// work-memory budget.
    pub fn all(child: Box<dyn PhysicalOperator + 'a>) -> Self {
        Self::all_with_work_mem(child, DEFAULT_DISTINCT_WORK_MEM_BYTES)
    }

    /// Construct a bounded full-row `DISTINCT` with an explicit byte budget.
    pub fn all_with_work_mem(child: Box<dyn PhysicalOperator + 'a>, work_mem_bytes: usize) -> Self {
        let schema = child.row_schema().clone();
        Self {
            child,
            keys: None,
            evaluator: None,
            schema,
            work_mem_bytes,
            spill_directory: None,
            seen: SeenKeySet::new(work_mem_bytes, None),
        }
    }

    /// Construct a bounded `DISTINCT ON` with the compatibility default
    /// work-memory budget.
    pub fn on(
        child: Box<dyn PhysicalOperator + 'a>,
        keys: Vec<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        Self::on_with_work_mem(child, keys, evaluator, DEFAULT_DISTINCT_WORK_MEM_BYTES)
    }

    /// Construct a bounded `DISTINCT ON` with an explicit byte budget.
    pub fn on_with_work_mem(
        child: Box<dyn PhysicalOperator + 'a>,
        keys: Vec<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        work_mem_bytes: usize,
    ) -> Self {
        let schema = child.row_schema().clone();
        Self {
            child,
            keys: Some(keys),
            evaluator: Some(evaluator),
            schema,
            work_mem_bytes,
            spill_directory: None,
            seen: SeenKeySet::new(work_mem_bytes, None),
        }
    }

    /// Place the exact-set files in a caller-selected temporary-data
    /// directory. The directory must already exist; a private child directory
    /// is created lazily on the first spill and removed through RAII.
    pub fn with_spill_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.spill_directory = Some(directory.into());
        self.reset_seen();
        self
    }

    /// Whether this invocation has migrated its key set to disk.
    pub fn has_spilled(&self) -> bool {
        self.seen.has_spilled()
    }

    /// Exact encoded key bytes retained by the in-memory set.
    pub fn in_memory_key_bytes(&self) -> usize {
        self.seen.in_memory_bytes()
    }

    /// Live private spill directory, for diagnostics and cleanup tests.
    pub fn spill_path(&self) -> Option<&Path> {
        self.seen.spill_path()
    }

    fn reset_seen(&mut self) {
        self.seen = SeenKeySet::new(self.work_mem_bytes, self.spill_directory.clone());
    }

    fn key(&self, schema: &RowSchema, row: &crate::PhysicalRow) -> ExecResult<Vec<u8>> {
        if let Some(keys) = self.keys.as_ref() {
            let evaluator = self.evaluator.as_ref().ok_or_else(|| {
                ExecError::Other("DISTINCT ON evaluator is not configured".into())
            })?;
            let values = keys
                .iter()
                .map(|expression| evaluator.evaluate_physical(expression, schema, row))
                .collect::<ExecResult<Vec<_>>>()?;
            return encode_key(&values);
        }
        let row = schema.view(row);
        encode_key_borrowed((0..self.schema.len()).map(|index| row.value_at(index)))
    }
}

impl PhysicalOperator for Distinct<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn open(&mut self) -> ExecResult<()> {
        self.reset_seen();
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        loop {
            let Some(batch) = self.child.next()? else {
                return Ok(None);
            };
            if batch.schema != self.schema {
                return Err(ExecError::Other(format!(
                    "DISTINCT input schema mismatch: expected {:?}, got {:?}",
                    self.schema, batch.schema
                )));
            }
            let mut rows = Vec::with_capacity(batch.rows.len());
            for row in batch.rows {
                let key = self.key(&batch.schema, &row)?;
                if self.seen.insert(key)? {
                    rows.push(row.without_lock_origins());
                }
            }
            if !rows.is_empty() {
                return Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)));
            }
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.reset_seen();
        self.child.close()
    }
}

#[cfg(test)]
mod tests;
