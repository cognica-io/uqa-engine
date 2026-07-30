//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical relational join operators.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;

use tempfile::{Builder as TempBuilder, NamedTempFile, TempDir};
use uqa_core::Value;
use uqa_sql::ast::JoinKind;
use uqa_sql::expr::truthy;
use uqa_sql::ResultRow;

use crate::{
    Batch, ExecError, ExecResult, IndexedSpill, PhysicalOperator, RowSchema, ScalarExpr,
    SharedExpressionEvaluator, SpillBuffer,
};

const DEFAULT_JOIN_WORK_MEM_BYTES: usize = 64 * 1024 * 1024;
const HASH_BUCKETS: u64 = 64;

fn output_schema(
    left: &[String],
    right: &[String],
    left_nulls: &ResultRow,
    right_nulls: &ResultRow,
) -> RowSchema {
    let mut columns = left.to_vec();
    for column in right
        .iter()
        .chain(left_nulls.keys())
        .chain(right_nulls.keys())
    {
        if !columns.contains(column) {
            columns.push(column.clone());
        }
    }
    RowSchema::new(columns)
}

fn merge_rows(left: &ResultRow, right: &ResultRow) -> ResultRow {
    let mut output = left.clone();
    for (column, value) in right {
        output.insert(column.clone(), value.clone());
    }
    output
}

fn merge_with_nulls(row: &ResultRow, nulls: &ResultRow, row_is_left: bool) -> ResultRow {
    if row_is_left {
        merge_rows(row, nulls)
    } else {
        merge_rows(nulls, row)
    }
}

fn push_output_row(
    output: &mut SpillBuffer,
    pending: &mut Vec<ResultRow>,
    schema: &RowSchema,
    row: ResultRow,
) -> ExecResult<()> {
    pending.push(row);
    if pending.len() == crate::batch::DEFAULT_BATCH_SIZE {
        output.push(Batch::new(schema.clone(), std::mem::take(pending)))?;
        pending.reserve(crate::batch::DEFAULT_BATCH_SIZE);
    }
    Ok(())
}

fn join_io_error(operation: &str, error: impl std::fmt::Display) -> ExecError {
    ExecError::Other(format!("join spill {operation}: {error}"))
}

/// One byte per build-side row, held in a temporary file rather than a
/// cardinality-sized `Vec<bool>`. Random updates are required for RIGHT/FULL
/// joins and remain constant-memory.
struct MatchFlags {
    file: NamedTempFile,
    rows: u64,
}

impl MatchFlags {
    fn new(rows: u64) -> ExecResult<Self> {
        let file =
            NamedTempFile::new().map_err(|error| join_io_error("create match flags", error))?;
        file.as_file()
            .set_len(rows)
            .map_err(|error| join_io_error("size match flags", error))?;
        Ok(Self { file, rows })
    }

    fn mark(&mut self, index: u64) -> ExecResult<()> {
        self.check_index(index)?;
        self.file
            .as_file_mut()
            .seek(SeekFrom::Start(index))
            .map_err(|error| join_io_error("seek match flags", error))?;
        self.file
            .as_file_mut()
            .write_all(&[1])
            .map_err(|error| join_io_error("write match flag", error))
    }

    fn is_marked(&mut self, index: u64) -> ExecResult<bool> {
        self.check_index(index)?;
        self.file
            .as_file_mut()
            .seek(SeekFrom::Start(index))
            .map_err(|error| join_io_error("seek match flags", error))?;
        let mut flag = [0_u8; 1];
        self.file
            .as_file_mut()
            .read_exact(&mut flag)
            .map_err(|error| join_io_error("read match flag", error))?;
        Ok(flag[0] != 0)
    }

    fn check_index(&self, index: u64) -> ExecResult<()> {
        if index >= self.rows {
            return Err(ExecError::Other(format!(
                "join match flag {index} is outside 0..{}",
                self.rows
            )));
        }
        Ok(())
    }
}

/// Exact, work-memory-bounded build-side hash index. It starts in memory and
/// migrates atomically to bucketed temporary files before the encoded key and
/// row-index records would exceed its byte budget. Disk probes always compare
/// the full key, so bucket hash collisions cannot create false join matches.
struct HybridHashIndex {
    memory: HashMap<Vec<u8>, Vec<u64>>,
    memory_bytes: usize,
    budget_bytes: usize,
    disk: Option<DiskHashIndex>,
}

impl HybridHashIndex {
    fn new(budget_bytes: usize) -> Self {
        Self {
            memory: HashMap::new(),
            memory_bytes: 0,
            budget_bytes,
            disk: None,
        }
    }

    fn insert(&mut self, key: Vec<u8>, row_index: u64) -> ExecResult<()> {
        if let Some(disk) = self.disk.as_mut() {
            return disk.insert(&key, row_index);
        }

        let record_bytes = key
            .len()
            .checked_add(16)
            .ok_or_else(|| ExecError::Other("join hash-index size overflow".into()))?;
        let fits = self
            .memory_bytes
            .checked_add(record_bytes)
            .is_some_and(|bytes| bytes <= self.budget_bytes);
        if fits {
            self.memory.entry(key).or_default().push(row_index);
            self.memory_bytes += record_bytes;
            return Ok(());
        }

        let mut disk = DiskHashIndex::new(None)?;
        for (existing_key, indices) in &self.memory {
            for index in indices {
                disk.insert(existing_key, *index)?;
            }
        }
        disk.insert(&key, row_index)?;
        self.memory.clear();
        self.memory_bytes = 0;
        self.disk = Some(disk);
        Ok(())
    }

    fn for_each_match(
        &mut self,
        key: &[u8],
        visitor: &mut dyn FnMut(u64) -> ExecResult<()>,
    ) -> ExecResult<bool> {
        if let Some(disk) = self.disk.as_mut() {
            return disk.for_each_match(key, visitor);
        }
        let Some(indices) = self.memory.get(key) else {
            return Ok(false);
        };
        for index in indices {
            visitor(*index)?;
        }
        Ok(!indices.is_empty())
    }

    fn has_spilled(&self) -> bool {
        self.disk.is_some()
    }
}

/// Bucket records are `[key_len: u64][key bytes][row_index: u64]`.
struct DiskHashIndex {
    directory: TempDir,
    buckets: BTreeMap<u8, File>,
}

impl DiskHashIndex {
    fn new(parent: Option<&Path>) -> ExecResult<Self> {
        let mut builder = TempBuilder::new();
        builder.prefix("uqa-hash-join-");
        let directory = parent
            .map_or_else(|| builder.tempdir(), |parent| builder.tempdir_in(parent))
            .map_err(|error| join_io_error("create hash directory", error))?;
        Ok(Self {
            directory,
            buckets: BTreeMap::new(),
        })
    }

    fn insert(&mut self, key: &[u8], row_index: u64) -> ExecResult<()> {
        let bucket = u8::try_from(stable_hash(key) % HASH_BUCKETS)
            .map_err(|_| ExecError::Other("join spill bucket exceeds u8".into()))?;
        if !self.buckets.contains_key(&bucket) {
            let path = self.directory.path().join(format!("bucket-{bucket:02x}"));
            let file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|error| join_io_error("create hash bucket", error))?;
            self.buckets.insert(bucket, file);
        }
        let file = self
            .buckets
            .get_mut(&bucket)
            .ok_or_else(|| ExecError::Other("join hash bucket registration failed".into()))?;
        let original_len = file
            .seek(SeekFrom::End(0))
            .map_err(|error| join_io_error("seek hash bucket", error))?;
        let key_len = u64::try_from(key.len())
            .map_err(|_| ExecError::Other("join hash key is too large".into()))?;
        let write_result = (|| -> std::io::Result<()> {
            file.write_all(&key_len.to_le_bytes())?;
            file.write_all(key)?;
            file.write_all(&row_index.to_le_bytes())?;
            file.flush()
        })();
        if let Err(error) = write_result {
            if let Err(rollback) = file.set_len(original_len) {
                return Err(ExecError::Other(format!(
                    "join spill append hash bucket: {error}; rollback failed: {rollback}"
                )));
            }
            return Err(join_io_error("append hash bucket", error));
        }
        Ok(())
    }

    fn for_each_match(
        &mut self,
        key: &[u8],
        visitor: &mut dyn FnMut(u64) -> ExecResult<()>,
    ) -> ExecResult<bool> {
        let bucket = u8::try_from(stable_hash(key) % HASH_BUCKETS)
            .map_err(|_| ExecError::Other("join spill bucket exceeds u8".into()))?;
        let Some(file) = self.buckets.get_mut(&bucket) else {
            return Ok(false);
        };
        file.seek(SeekFrom::Start(0))
            .map_err(|error| join_io_error("rewind hash bucket", error))?;
        let file_len = file
            .metadata()
            .map_err(|error| join_io_error("inspect hash bucket", error))?
            .len();
        let mut matched = false;
        while let Some(key_len) = read_u64(file, "read hash key length")? {
            let key_start = file
                .stream_position()
                .map_err(|error| join_io_error("locate hash key", error))?;
            let key_end = key_start
                .checked_add(key_len)
                .ok_or_else(|| ExecError::Other("join hash key offset overflow".into()))?;
            let record_end = key_end
                .checked_add(8)
                .ok_or_else(|| ExecError::Other("join hash record offset overflow".into()))?;
            if record_end > file_len {
                return Err(ExecError::Other(format!(
                    "join hash key length {key_len} exceeds remaining bucket record bytes"
                )));
            }
            let key_matches = compare_hash_key(file, key_start, key_end, key_len, key)?;
            let row_index = read_u64(file, "read hash row index")?
                .ok_or_else(|| ExecError::Other("truncated join hash row index".into()))?;
            if key_matches {
                visitor(row_index)?;
                matched = true;
            }
        }
        Ok(matched)
    }
}

fn compare_hash_key(
    file: &mut File,
    key_start: u64,
    key_end: u64,
    stored_len: u64,
    expected: &[u8],
) -> ExecResult<bool> {
    let expected_len = u64::try_from(expected.len())
        .map_err(|_| ExecError::Other("join probe key length is invalid".into()))?;
    if stored_len != expected_len {
        file.seek(SeekFrom::Start(key_end))
            .map_err(|error| join_io_error("skip non-matching hash key", error))?;
        return Ok(false);
    }

    file.seek(SeekFrom::Start(key_start))
        .map_err(|error| join_io_error("seek hash key", error))?;
    let mut buffer = [0_u8; 8 * 1024];
    let mut compared = 0_usize;
    let mut matches = true;
    while compared < expected.len() {
        let take = (expected.len() - compared).min(buffer.len());
        file.read_exact(&mut buffer[..take])
            .map_err(|error| join_io_error("read hash key", error))?;
        matches &= buffer[..take] == expected[compared..compared + take];
        compared += take;
    }
    Ok(matches)
}

fn read_u64(file: &mut File, operation: &str) -> ExecResult<Option<u64>> {
    let mut encoded = [0_u8; 8];
    match file.read(&mut encoded[..1]) {
        Ok(0) => return Ok(None),
        Ok(1) => {}
        Ok(count) => {
            return Err(ExecError::Other(format!(
                "invalid join spill read count: requested 1 byte, received {count}"
            )));
        }
        Err(error) => return Err(join_io_error(operation, error)),
    }
    file.read_exact(&mut encoded[1..]).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            ExecError::Other(format!(
                "truncated join spill record while attempting to {operation}"
            ))
        } else {
            join_io_error(operation, error)
        }
    })?;
    Ok(Some(u64::from_le_bytes(encoded)))
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Nested-loop implementation for arbitrary join predicates and every SQL
/// outer-join shape. Predicate evaluation happens against the merged row, so
/// qualified columns and engine-provided scalar/subquery semantics remain
/// available through the shared expression evaluator.
pub struct NestedLoopJoin<'a> {
    left: Box<dyn PhysicalOperator + 'a>,
    right: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    predicate: Option<ScalarExpr>,
    evaluator: SharedExpressionEvaluator<'a>,
    left_nulls: ResultRow,
    right_nulls: ResultRow,
    schema: RowSchema,
    work_mem_bytes: usize,
    output: Option<crate::spill::SpillDrain>,
    output_spilled: bool,
    right_input_spilled: bool,
}

impl<'a> NestedLoopJoin<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        predicate: Option<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
    ) -> Self {
        Self::new_with_work_mem(
            left,
            right,
            kind,
            predicate,
            evaluator,
            left_nulls,
            right_nulls,
            DEFAULT_JOIN_WORK_MEM_BYTES,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_work_mem(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        predicate: Option<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
        work_mem_bytes: usize,
    ) -> Self {
        let schema = output_schema(left.schema(), right.schema(), &left_nulls, &right_nulls);
        Self {
            left,
            right,
            kind,
            predicate,
            evaluator,
            left_nulls,
            right_nulls,
            schema,
            work_mem_bytes,
            output: None,
            output_spilled: false,
            right_input_spilled: false,
        }
    }

    pub fn output_has_spilled(&self) -> bool {
        self.output_spilled
    }

    /// The repeatable nested-loop build input is kept in an indexed temporary
    /// row store, never in a cardinality-sized in-memory vector.
    pub fn right_input_has_spilled(&self) -> bool {
        self.right_input_spilled
    }

    fn matches(&self, row: &ResultRow) -> ExecResult<bool> {
        match self.predicate.as_ref() {
            None => Ok(true),
            Some(predicate) => Ok(truthy(&self.evaluator.evaluate(predicate, row)?)),
        }
    }
}

impl PhysicalOperator for NestedLoopJoin<'_> {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.output = None;
        self.output_spilled = false;
        self.right_input_spilled = false;

        let mut right = IndexedSpill::new()?;
        self.right.open()?;
        while let Some(batch) = self.right.next()? {
            for row in batch.rows {
                right.push(&row)?;
            }
        }
        self.right_input_spilled = !right.is_empty();
        let mut matched_right = MatchFlags::new(right.len())?;
        let mut output = SpillBuffer::new(self.work_mem_bytes.max(1));
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);

        self.left.open()?;
        while let Some(batch) = self.left.next()? {
            for left_row in batch.rows {
                let mut matched_left = false;
                for right_index in 0..right.len() {
                    let right_row = right.get(right_index)?;
                    let merged = merge_rows(&left_row, &right_row);
                    if self.matches(&merged)? {
                        push_output_row(&mut output, &mut pending, &self.schema, merged)?;
                        matched_left = true;
                        matched_right.mark(right_index)?;
                    }
                }
                if !matched_left && matches!(self.kind, JoinKind::Left | JoinKind::Full) {
                    push_output_row(
                        &mut output,
                        &mut pending,
                        &self.schema,
                        merge_with_nulls(&left_row, &self.right_nulls, true),
                    )?;
                }
            }
        }

        if matches!(self.kind, JoinKind::Right | JoinKind::Full) {
            for right_index in 0..right.len() {
                if !matched_right.is_marked(right_index)? {
                    let right_row = right.get(right_index)?;
                    push_output_row(
                        &mut output,
                        &mut pending,
                        &self.schema,
                        merge_with_nulls(&right_row, &self.left_nulls, false),
                    )?;
                }
            }
        }
        if !pending.is_empty() {
            output.push(Batch::new(self.schema.clone(), pending))?;
        }
        self.output_spilled = output.has_spilled();
        self.output = Some(output.drain()?);
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.output
            .as_mut()
            .map_or(Ok(None), |output| output.next().transpose())
    }

    fn close(&mut self) -> ExecResult<()> {
        self.output = None;
        let left = self.left.close();
        let right = self.right.close();
        crate::physical::with_cleanup(left, right, "close right nested-loop join input")
    }
}

/// Equality join backed by a canonical SQL-key hash table. SQL NULL keys never
/// match. An optional residual predicate is evaluated on hash candidates
/// before either side is marked matched, preserving mixed equijoin/non-equality
/// `ON` semantics for every outer-join shape.
pub struct HashJoin<'a> {
    left: Box<dyn PhysicalOperator + 'a>,
    right: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    left_keys: Vec<ScalarExpr>,
    right_keys: Vec<ScalarExpr>,
    predicate: Option<ScalarExpr>,
    evaluator: SharedExpressionEvaluator<'a>,
    left_nulls: ResultRow,
    right_nulls: ResultRow,
    schema: RowSchema,
    work_mem_bytes: usize,
    output: Option<crate::spill::SpillDrain>,
    output_spilled: bool,
    right_input_spilled: bool,
    hash_index_spilled: bool,
}

impl<'a> HashJoin<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        left_keys: Vec<ScalarExpr>,
        right_keys: Vec<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
    ) -> Self {
        Self::new_with_work_mem(
            left,
            right,
            kind,
            left_keys,
            right_keys,
            evaluator,
            left_nulls,
            right_nulls,
            DEFAULT_JOIN_WORK_MEM_BYTES,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_work_mem(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        left_keys: Vec<ScalarExpr>,
        right_keys: Vec<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
        work_mem_bytes: usize,
    ) -> Self {
        Self::new_with_work_mem_and_predicate(
            left,
            right,
            kind,
            left_keys,
            right_keys,
            None,
            evaluator,
            left_nulls,
            right_nulls,
            work_mem_bytes,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_work_mem_and_predicate(
        left: Box<dyn PhysicalOperator + 'a>,
        right: Box<dyn PhysicalOperator + 'a>,
        kind: JoinKind,
        left_keys: Vec<ScalarExpr>,
        right_keys: Vec<ScalarExpr>,
        predicate: Option<ScalarExpr>,
        evaluator: SharedExpressionEvaluator<'a>,
        left_nulls: ResultRow,
        right_nulls: ResultRow,
        work_mem_bytes: usize,
    ) -> Self {
        let schema = output_schema(left.schema(), right.schema(), &left_nulls, &right_nulls);
        Self {
            left,
            right,
            kind,
            left_keys,
            right_keys,
            predicate,
            evaluator,
            left_nulls,
            right_nulls,
            schema,
            work_mem_bytes,
            output: None,
            output_spilled: false,
            right_input_spilled: false,
            hash_index_spilled: false,
        }
    }

    pub fn output_has_spilled(&self) -> bool {
        self.output_spilled
    }

    pub fn right_input_has_spilled(&self) -> bool {
        self.right_input_spilled
    }

    pub fn hash_index_has_spilled(&self) -> bool {
        self.hash_index_spilled
    }

    fn key(&self, expressions: &[ScalarExpr], row: &ResultRow) -> ExecResult<Option<Vec<u8>>> {
        let mut values = Vec::with_capacity(expressions.len());
        for expression in expressions {
            let value = self.evaluator.evaluate(expression, row)?;
            if matches!(value, Value::Null) {
                return Ok(None);
            }
            values.push(value);
        }
        crate::distinct::encode_key(&values).map(Some)
    }

    fn matches(&self, row: &ResultRow) -> ExecResult<bool> {
        self.predicate.as_ref().map_or(Ok(true), |predicate| {
            Ok(truthy(&self.evaluator.evaluate(predicate, row)?))
        })
    }
}

impl PhysicalOperator for HashJoin<'_> {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn open(&mut self) -> ExecResult<()> {
        self.output = None;
        self.output_spilled = false;
        self.right_input_spilled = false;
        self.hash_index_spilled = false;

        let state_budget = (self.work_mem_bytes / 2).max(1);
        let output_budget = self.work_mem_bytes.saturating_sub(state_budget).max(1);
        let mut right = IndexedSpill::new()?;
        let mut right_by_key = HybridHashIndex::new(state_budget);
        self.right.open()?;
        while let Some(batch) = self.right.next()? {
            for row in batch.rows {
                let index = right.len();
                if let Some(key) = self.key(&self.right_keys, &row)? {
                    right_by_key.insert(key, index)?;
                }
                right.push(&row)?;
            }
        }
        self.right_input_spilled = !right.is_empty();
        self.hash_index_spilled = right_by_key.has_spilled();

        let mut matched_right = MatchFlags::new(right.len())?;
        let mut output = SpillBuffer::new(output_budget);
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);

        self.left.open()?;
        while let Some(batch) = self.left.next()? {
            for left_row in batch.rows {
                let mut matched_left = false;
                if let Some(key) = self.key(&self.left_keys, &left_row)? {
                    right_by_key.for_each_match(&key, &mut |index| {
                        let right_row = right.get(index)?;
                        let merged = merge_rows(&left_row, &right_row);
                        if self.matches(&merged)? {
                            push_output_row(&mut output, &mut pending, &self.schema, merged)?;
                            matched_right.mark(index)?;
                            matched_left = true;
                        }
                        Ok(())
                    })?;
                }
                if !matched_left && matches!(self.kind, JoinKind::Left | JoinKind::Full) {
                    push_output_row(
                        &mut output,
                        &mut pending,
                        &self.schema,
                        merge_with_nulls(&left_row, &self.right_nulls, true),
                    )?;
                }
            }
        }

        if matches!(self.kind, JoinKind::Right | JoinKind::Full) {
            for index in 0..right.len() {
                if !matched_right.is_marked(index)? {
                    let right_row = right.get(index)?;
                    push_output_row(
                        &mut output,
                        &mut pending,
                        &self.schema,
                        merge_with_nulls(&right_row, &self.left_nulls, false),
                    )?;
                }
            }
        }
        if !pending.is_empty() {
            output.push(Batch::new(self.schema.clone(), pending))?;
        }
        self.output_spilled = output.has_spilled();
        self.output = Some(output.drain()?);
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.output
            .as_mut()
            .map_or(Ok(None), |output| output.next().transpose())
    }

    fn close(&mut self) -> ExecResult<()> {
        self.output = None;
        let left = self.left.close();
        let right = self.right.close();
        crate::physical::with_cleanup(left, right, "close right hash-join input")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::physical::run_to_rows;
    use crate::scan::TableScan;

    fn row(values: &[(&str, Value)]) -> ResultRow {
        values
            .iter()
            .map(|(column, value)| ((*column).to_string(), value.clone()))
            .collect()
    }

    fn evaluator() -> SharedExpressionEvaluator<'static> {
        Arc::new(TestEvaluator)
    }

    #[test]
    fn disk_hash_index_rejects_corrupt_key_length_without_allocating_it() {
        let key = b"join-key";
        let mut index = DiskHashIndex::new(None).unwrap();
        index.insert(key, 7).unwrap();
        let bucket = u8::try_from(stable_hash(key) % HASH_BUCKETS).unwrap();
        let file = index.buckets.get_mut(&bucket).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&(1_u64 << 40).to_le_bytes()).unwrap();
        file.flush().unwrap();

        let error = index.for_each_match(key, &mut |_| Ok(())).unwrap_err();
        assert!(error
            .to_string()
            .contains("exceeds remaining bucket record bytes"));
    }

    struct TestEvaluator;

    impl crate::ExpressionEvaluator for TestEvaluator {
        fn evaluate(&self, expression: &ScalarExpr, row: &ResultRow) -> ExecResult<Value> {
            Ok(crate::eval_scalar(
                expression,
                &crate::ScalarEvalContext::new(Some(row), &[]),
            )?)
        }
    }

    #[test]
    fn hash_full_join_preserves_unmatched_rows() {
        let left = TableScan::from_rows(
            vec!["l.id".into()],
            vec![
                row(&[("l.id", Value::Int(1))]),
                row(&[("l.id", Value::Int(2))]),
            ],
        );
        let right = TableScan::from_rows(
            vec!["r.id".into()],
            vec![
                row(&[("r.id", Value::Int(2))]),
                row(&[("r.id", Value::Int(3))]),
            ],
        );
        let mut join = HashJoin::new(
            Box::new(left),
            Box::new(right),
            JoinKind::Full,
            vec![ScalarExpr::Column("l.id".into())],
            vec![ScalarExpr::Column("r.id".into())],
            evaluator(),
            row(&[("l.id", Value::Null)]),
            row(&[("r.id", Value::Null)]),
        );
        let (_, rows) = run_to_rows(&mut join).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows
            .iter()
            .any(|row| row["l.id"] == Value::Int(1) && row["r.id"] == Value::Null));
        assert!(rows
            .iter()
            .any(|row| row["l.id"] == Value::Int(2) && row["r.id"] == Value::Int(2)));
        assert!(rows
            .iter()
            .any(|row| row["l.id"] == Value::Null && row["r.id"] == Value::Int(3)));
    }

    #[test]
    fn hash_join_applies_residual_predicate_before_marking_matches() {
        let left = TableScan::from_rows(
            vec!["l.k".into(), "l.v".into()],
            vec![
                row(&[("l.k", Value::Int(1)), ("l.v", Value::Int(1))]),
                row(&[("l.k", Value::Int(1)), ("l.v", Value::Int(3))]),
            ],
        );
        let right = TableScan::from_rows(
            vec!["r.k".into(), "r.v".into()],
            vec![row(&[("r.k", Value::Int(1)), ("r.v", Value::Int(2))])],
        );
        let predicate = ScalarExpr::Binary {
            op: uqa_sql::ast::BinaryOp::Greater,
            lhs: Box::new(ScalarExpr::Column("l.v".into())),
            rhs: Box::new(ScalarExpr::Column("r.v".into())),
        };
        let mut join = HashJoin::new_with_work_mem_and_predicate(
            Box::new(left),
            Box::new(right),
            JoinKind::Full,
            vec![ScalarExpr::Column("l.k".into())],
            vec![ScalarExpr::Column("r.k".into())],
            Some(predicate),
            evaluator(),
            row(&[("l.k", Value::Null), ("l.v", Value::Null)]),
            row(&[("r.k", Value::Null), ("r.v", Value::Null)]),
            1,
        );
        let (_, rows) = run_to_rows(&mut join).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| {
            row["l.v"] == Value::Int(1) && row["r.k"] == Value::Null && row["r.v"] == Value::Null
        }));
        assert!(rows
            .iter()
            .any(|row| row["l.v"] == Value::Int(3) && row["r.v"] == Value::Int(2)));
    }

    #[test]
    fn nested_loop_predicate_errors_propagate() {
        struct FailingEvaluator;
        impl crate::ExpressionEvaluator for FailingEvaluator {
            fn evaluate(&self, _: &ScalarExpr, _: &ResultRow) -> ExecResult<Value> {
                Err(crate::ExecError::Other("join predicate failed".into()))
            }
        }
        let left = TableScan::from_rows(vec!["l".into()], vec![row(&[("l", Value::Int(1))])]);
        let right = TableScan::from_rows(vec!["r".into()], vec![row(&[("r", Value::Int(1))])]);
        let mut join = NestedLoopJoin::new(
            Box::new(left),
            Box::new(right),
            JoinKind::Inner,
            Some(ScalarExpr::Literal(Value::Bool(true))),
            Arc::new(FailingEvaluator),
            ResultRow::new(),
            ResultRow::new(),
        );
        let error = run_to_rows(&mut join).unwrap_err();
        assert!(error.to_string().contains("join predicate failed"));
    }

    #[test]
    fn high_cardinality_hash_join_spills_output() {
        let left = TableScan::from_rows(
            vec!["l.k".into(), "l.id".into()],
            (0..48)
                .map(|id| row(&[("l.k", Value::Int(1)), ("l.id", Value::Int(id))]))
                .collect(),
        );
        let right = TableScan::from_rows(
            vec!["r.k".into(), "r.id".into()],
            (0..48)
                .map(|id| row(&[("r.k", Value::Int(1)), ("r.id", Value::Int(id))]))
                .collect(),
        );
        let mut join = HashJoin::new_with_work_mem(
            Box::new(left),
            Box::new(right),
            JoinKind::Inner,
            vec![ScalarExpr::Column("l.k".into())],
            vec![ScalarExpr::Column("r.k".into())],
            evaluator(),
            ResultRow::new(),
            ResultRow::new(),
            1,
        );
        let (_, rows) = run_to_rows(&mut join).unwrap();
        assert!(join.output_has_spilled());
        assert!(join.right_input_has_spilled());
        assert!(join.hash_index_has_spilled());
        assert_eq!(rows.len(), 48 * 48);
    }

    #[test]
    fn high_cardinality_nested_loop_join_spills_output() {
        let left = TableScan::from_rows(
            vec!["l.id".into()],
            (0..48).map(|id| row(&[("l.id", Value::Int(id))])).collect(),
        );
        let right = TableScan::from_rows(
            vec!["r.id".into()],
            (0..48).map(|id| row(&[("r.id", Value::Int(id))])).collect(),
        );
        let mut join = NestedLoopJoin::new_with_work_mem(
            Box::new(left),
            Box::new(right),
            JoinKind::Cross,
            None,
            evaluator(),
            ResultRow::new(),
            ResultRow::new(),
            1,
        );
        let (_, rows) = run_to_rows(&mut join).unwrap();
        assert!(join.output_has_spilled());
        assert!(join.right_input_has_spilled());
        assert_eq!(rows.len(), 48 * 48);
    }

    struct GeneratedRows {
        schema: Vec<String>,
        prefix: &'static str,
        next: i64,
        end: i64,
    }

    impl crate::RowSource for GeneratedRows {
        fn schema(&self) -> &[String] {
            &self.schema
        }

        fn next_row(&mut self) -> ExecResult<Option<ResultRow>> {
            if self.next == self.end {
                return Ok(None);
            }
            let value = self.next;
            self.next += 1;
            Ok(Some(row(&[(self.prefix, Value::Int(value))])))
        }
    }

    #[test]
    fn tiny_work_mem_hash_join_streams_left_and_spills_distinct_build_keys() {
        const ROWS: i64 = 2_048;
        let left = TableScan::new(Box::new(GeneratedRows {
            schema: vec!["l.k".into()],
            prefix: "l.k",
            next: 0,
            end: ROWS,
        }));
        let right = TableScan::new(Box::new(GeneratedRows {
            schema: vec!["r.k".into()],
            prefix: "r.k",
            next: 0,
            end: ROWS,
        }));
        let mut join = HashJoin::new_with_work_mem(
            Box::new(left),
            Box::new(right),
            JoinKind::Inner,
            vec![ScalarExpr::Column("l.k".into())],
            vec![ScalarExpr::Column("r.k".into())],
            evaluator(),
            ResultRow::new(),
            ResultRow::new(),
            1,
        );
        let (_, rows) = run_to_rows(&mut join).unwrap();
        assert_eq!(rows.len(), ROWS as usize);
        assert!(join.right_input_has_spilled());
        assert!(join.hash_index_has_spilled());
        assert!(join.output_has_spilled());
    }

    struct FailingRows {
        schema: Vec<String>,
        emitted: bool,
    }

    impl crate::RowSource for FailingRows {
        fn schema(&self) -> &[String] {
            &self.schema
        }

        fn next_row(&mut self) -> ExecResult<Option<ResultRow>> {
            if self.emitted {
                return Err(ExecError::Other("injected join input failure".into()));
            }
            self.emitted = true;
            Ok(Some(row(&[("r.k", Value::Int(1))])))
        }
    }

    #[test]
    fn build_side_input_error_is_propagated_before_any_join_result() {
        let left = TableScan::from_rows(vec!["l.k".into()], vec![row(&[("l.k", Value::Int(1))])]);
        let right = TableScan::new(Box::new(FailingRows {
            schema: vec!["r.k".into()],
            emitted: false,
        }));
        let mut join = HashJoin::new_with_work_mem(
            Box::new(left),
            Box::new(right),
            JoinKind::Inner,
            vec![ScalarExpr::Column("l.k".into())],
            vec![ScalarExpr::Column("r.k".into())],
            evaluator(),
            ResultRow::new(),
            ResultRow::new(),
            1,
        );
        let error = run_to_rows(&mut join).unwrap_err();
        assert!(error.to_string().contains("injected join input failure"));
    }
}
