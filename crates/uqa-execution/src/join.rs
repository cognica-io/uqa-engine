//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical relational join operators.

mod nested_loop;

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::hash::BuildHasher;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;

use smallvec::SmallVec;
use tempfile::{Builder as TempBuilder, NamedTempFile, TempDir};
use uqa_core::Value;
use uqa_sql::ast::JoinKind;
use uqa_sql::expr::truthy;
use uqa_sql::ResultRow;

use crate::distinct::{encode_non_null_key, hash_canonical_row, EncodedKey};
use crate::{
    Batch, ExecError, ExecResult, IndexedSpill, PhysicalOperator, PhysicalRow, ProjectedPredicate,
    RowSchema, ScalarExpr, SharedExpressionEvaluator, SpillBuffer,
};

pub use nested_loop::NestedLoopJoin;

const DEFAULT_JOIN_WORK_MEM_BYTES: usize = 64 * 1024 * 1024;
const HASH_BUCKETS: u64 = 64;

fn output_schema(
    left: &RowSchema,
    right: &RowSchema,
    left_nulls: &ResultRow,
    right_nulls: &ResultRow,
) -> RowSchema {
    RowSchema::join(
        left,
        right,
        left_nulls.keys().chain(right_nulls.keys()).cloned(),
    )
}

fn push_output_row(
    output: &mut SpillBuffer,
    pending: &mut Vec<PhysicalRow>,
    schema: &RowSchema,
    row: PhysicalRow,
) -> ExecResult<()> {
    pending.push(row);
    if pending.len() == crate::batch::DEFAULT_BATCH_SIZE {
        output.push(Batch::from_physical_rows(
            schema.clone(),
            std::mem::take(pending),
        ))?;
        pending.reserve(crate::batch::DEFAULT_BATCH_SIZE);
    }
    Ok(())
}

fn join_io_error(operation: &str, error: impl std::fmt::Display) -> ExecError {
    ExecError::Other(format!("join spill {operation}: {error}"))
}

/// Positional build-side row storage that only touches disk after its encoded
/// memory budget is exhausted. Once spilled, every row lives in the indexed
/// disk store so positional indices remain stable across the transition.
struct HybridRowStore {
    schema: RowSchema,
    memory: Vec<PhysicalRow>,
    rows: u64,
    memory_bytes: usize,
    budget_bytes: usize,
    disk: Option<IndexedSpill>,
}

impl HybridRowStore {
    fn new(schema: RowSchema, budget_bytes: usize) -> Self {
        Self {
            schema,
            memory: Vec::new(),
            rows: 0,
            memory_bytes: 0,
            budget_bytes,
            disk: None,
        }
    }

    fn len(&self) -> u64 {
        self.disk.as_ref().map_or(self.rows, IndexedSpill::len)
    }

    fn has_spilled(&self) -> bool {
        self.disk.is_some()
    }

    fn memory_row(&self, index: u64) -> Option<&PhysicalRow> {
        if self.disk.is_some() {
            return None;
        }
        usize::try_from(index)
            .ok()
            .and_then(|index| self.memory.get(index))
    }

    fn push(&mut self, row: PhysicalRow) -> ExecResult<()> {
        if let Some(disk) = self.disk.as_mut() {
            return disk.push(&row);
        }

        let row_bytes = IndexedSpill::encoded_row_size(&self.schema, &row)?;
        let next_rows = self
            .rows
            .checked_add(1)
            .ok_or_else(|| ExecError::Other("join build row count overflow".into()))?;
        let fits = self
            .memory_bytes
            .checked_add(row_bytes)
            .is_some_and(|bytes| bytes <= self.budget_bytes);
        if fits {
            self.memory.push(row);
            self.rows = next_rows;
            self.memory_bytes += row_bytes;
            return Ok(());
        }

        // Build the complete disk representation before publishing it. If any
        // append fails, the original in-memory rows remain available and the
        // operator aborts without exposing a partial positional store.
        let mut disk = IndexedSpill::new(self.schema.clone())?;
        for existing in &self.memory {
            disk.push(existing)?;
        }
        disk.push(&row)?;
        self.memory.clear();
        self.rows = disk.len();
        self.memory_bytes = 0;
        self.disk = Some(disk);
        Ok(())
    }

    fn with_row<T>(
        &mut self,
        index: u64,
        visitor: impl FnOnce(&PhysicalRow) -> ExecResult<T>,
    ) -> ExecResult<T> {
        if let Some(disk) = self.disk.as_mut() {
            let row = disk.get(index)?;
            return visitor(&row);
        }

        let index = usize::try_from(index)
            .map_err(|_| ExecError::Other(format!("join row index {index} exceeds usize")))?;
        let row = self.memory.get(index).ok_or_else(|| {
            ExecError::Other(format!(
                "join row {index} is outside 0..{}",
                self.memory.len()
            ))
        })?;
        visitor(row)
    }
}

/// Allocation-free in-memory index for simple positional equality keys.
///
/// Only the canonical hash and build-row position are retained. The key itself
/// stays in the original [`PhysicalRow`]; hash collisions are resolved by
/// comparing the mapped source slots. If the index exceeds its budget, its
/// state is discarded and the caller rebuilds the spill-capable encoded index
/// from the row store.
struct DirectHashIndex {
    buckets: HashMap<u64, SmallVec<[u64; 1]>, ahash::RandomState>,
    memory_bytes: usize,
    budget_bytes: usize,
    overflowed: bool,
}

impl DirectHashIndex {
    fn new(budget_bytes: usize) -> Self {
        Self {
            buckets: HashMap::with_hasher(ahash::RandomState::new()),
            memory_bytes: 0,
            budget_bytes,
            overflowed: false,
        }
    }

    fn hasher(&self) -> &ahash::RandomState {
        self.buckets.hasher()
    }

    fn insert(&mut self, hash: u64, row_index: u64) -> ExecResult<()> {
        if self.overflowed {
            return Ok(());
        }

        // Account for the hash, inline first row index, control bytes, and
        // allocator/table slack. Duplicate-key indices need only one u64.
        let record_bytes = if self.buckets.contains_key(&hash) {
            8
        } else {
            64
        };
        let fits = self
            .memory_bytes
            .checked_add(record_bytes)
            .is_some_and(|bytes| bytes <= self.budget_bytes);
        if !fits {
            self.buckets.clear();
            self.memory_bytes = 0;
            self.overflowed = true;
            return Ok(());
        }

        self.buckets.entry(hash).or_default().push(row_index);
        self.memory_bytes += record_bytes;
        Ok(())
    }

    fn is_available(&self) -> bool {
        !self.overflowed
    }

    fn candidates(&self, hash: u64) -> &[u64] {
        self.buckets.get(&hash).map_or(&[], SmallVec::as_slice)
    }

    fn keys_are_unique(
        &self,
        rows: &HybridRowStore,
        schema: &RowSchema,
        positions: &[usize],
    ) -> bool {
        self.is_available()
            && self.buckets.values().all(|bucket| {
                bucket.iter().enumerate().all(|(offset, left_index)| {
                    bucket[offset + 1..].iter().all(|right_index| {
                        let Some(left) = rows.memory_row(*left_index) else {
                            return false;
                        };
                        let Some(right) = rows.memory_row(*right_index) else {
                            return false;
                        };
                        !positional_keys_equal(schema, left, positions, schema, right, positions)
                    })
                })
            })
    }
}

fn positional_key_hash<S: BuildHasher>(
    build_hasher: &S,
    schema: &RowSchema,
    row: &PhysicalRow,
    positions: &[usize],
) -> ExecResult<Option<u64>> {
    let view = schema.view(row);
    if positions.iter().any(|position| {
        view.value_at(*position)
            .is_none_or(|value| matches!(value, Value::Null))
    }) {
        return Ok(None);
    }
    hash_canonical_row(
        build_hasher,
        positions.iter().map(|position| view.value_at(*position)),
    )
    .map(Some)
}

fn positional_keys_equal(
    left_schema: &RowSchema,
    left_row: &PhysicalRow,
    left_positions: &[usize],
    right_schema: &RowSchema,
    right_row: &PhysicalRow,
    right_positions: &[usize],
) -> bool {
    if left_positions.len() != right_positions.len() {
        return false;
    }
    let left = left_schema.view(left_row);
    let right = right_schema.view(right_row);
    left_positions
        .iter()
        .zip(right_positions)
        .all(|(left_position, right_position)| {
            let Some(left) = left.value_at(*left_position) else {
                return false;
            };
            let Some(right) = right.value_at(*right_position) else {
                return false;
            };
            !matches!(left, Value::Null) && !matches!(right, Value::Null) && left == right
        })
}

fn direct_unique_match(
    index: &DirectHashIndex,
    build_rows: &HybridRowStore,
    build_positions: &[usize],
    probe_schema: &RowSchema,
    probe_row: &PhysicalRow,
    probe_positions: &[usize],
) -> ExecResult<Option<u64>> {
    let Some(hash) = positional_key_hash(index.hasher(), probe_schema, probe_row, probe_positions)?
    else {
        return Ok(None);
    };
    Ok(index.candidates(hash).iter().copied().find(|row_index| {
        build_rows.memory_row(*row_index).is_some_and(|build_row| {
            positional_keys_equal(
                &build_rows.schema,
                build_row,
                build_positions,
                probe_schema,
                probe_row,
                probe_positions,
            )
        })
    }))
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
    memory: HashMap<EncodedKey, Vec<u64>, ahash::RandomState>,
    memory_bytes: usize,
    budget_bytes: usize,
    disk: Option<DiskHashIndex>,
}

#[derive(Clone, Copy)]
enum MemoryMatchSummary {
    Absent,
    Single(u64),
    Multiple,
}

impl HybridHashIndex {
    fn new(budget_bytes: usize) -> Self {
        Self {
            // AHash keeps a per-index random seed while avoiding SipHash's
            // cryptographic-round overhead after the canonical SQL key has
            // already been encoded byte-for-byte.
            memory: HashMap::with_hasher(ahash::RandomState::new()),
            memory_bytes: 0,
            budget_bytes,
            disk: None,
        }
    }

    fn insert(&mut self, key: EncodedKey, row_index: u64) -> ExecResult<()> {
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

    /// Summarize a probe without allocation when the index still resides in
    /// memory. Disk-backed indexes return `None` and use the streaming probe.
    fn memory_match_summary(&self, key: &[u8]) -> Option<MemoryMatchSummary> {
        if self.disk.is_some() {
            return None;
        }
        Some(match self.memory.get(key).map(Vec::as_slice) {
            None | Some([]) => MemoryMatchSummary::Absent,
            Some([index]) => MemoryMatchSummary::Single(*index),
            Some(_) => MemoryMatchSummary::Multiple,
        })
    }

    fn has_spilled(&self) -> bool {
        self.disk.is_some()
    }

    fn is_memory_unique(&self) -> bool {
        self.disk.is_none() && self.memory.values().all(|indices| indices.len() == 1)
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

/// Equality join backed by a canonical SQL-key hash table. SQL NULL keys never
/// match. An optional residual predicate is evaluated on hash candidates
/// before either side is marked matched, preserving mixed equijoin/non-equality
/// `ON` semantics for every outer-join shape.
fn simple_key_positions(schema: &RowSchema, expressions: &[ScalarExpr]) -> Option<Vec<usize>> {
    expressions
        .iter()
        .map(|expression| match expression {
            ScalarExpr::Column(column) => schema.position(column),
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                schema.qualified_position(qualifier, column)
            }
            _ => None,
        })
        .collect()
}

pub struct HashJoin<'a> {
    left: Box<dyn PhysicalOperator + 'a>,
    right: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    left_keys: Vec<ScalarExpr>,
    right_keys: Vec<ScalarExpr>,
    left_key_positions: Option<Vec<usize>>,
    right_key_positions: Option<Vec<usize>>,
    predicate: Option<ScalarExpr>,
    prepared_predicate: Option<ProjectedPredicate>,
    evaluator: SharedExpressionEvaluator<'a>,
    left_nulls: PhysicalRow,
    right_nulls: PhysicalRow,
    schema: RowSchema,
    estimated_cardinality: Option<u64>,
    build_left: bool,
    work_mem_bytes: usize,
    output: Option<crate::spill::SpillDrain>,
    streaming_unique: Option<UniqueHashJoinState>,
    output_spilled: SpillState,
    right_input_spilled: SpillState,
    hash_index_spilled: SpillState,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum SpillState {
    #[default]
    InMemory,
    Spilled,
}

impl SpillState {
    fn is_spilled(self) -> bool {
        matches!(self, Self::Spilled)
    }
}

impl From<bool> for SpillState {
    fn from(spilled: bool) -> Self {
        if spilled {
            Self::Spilled
        } else {
            Self::InMemory
        }
    }
}

/// State retained while an in-memory unique-key inner join streams its probe
/// side. At most one output row can be produced per probe row, so the join can
/// preserve batch backpressure without a cardinality-sized output buffer.
struct UniqueHashJoinState {
    build_rows: HybridRowStore,
    hash_index: UniqueHashIndex,
    build_left: bool,
}

enum UniqueHashIndex {
    /// Simple column keys keep only hashes and row positions. Candidate keys
    /// are verified against the original build row slots.
    Direct(DirectHashIndex),
    /// Evaluated expressions and spill fallback retain canonical byte keys.
    Encoded(HybridHashIndex),
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
        let left_key_positions = simple_key_positions(left.row_schema(), &left_keys);
        let right_key_positions = simple_key_positions(right.row_schema(), &right_keys);
        let schema = output_schema(
            left.row_schema(),
            right.row_schema(),
            &left_nulls,
            &right_nulls,
        );
        let left_nulls = PhysicalRow::nulls(left.row_schema().physical_width());
        let right_nulls = PhysicalRow::nulls(right.row_schema().physical_width());
        let left_cardinality = left.estimated_cardinality();
        let right_cardinality = right.estimated_cardinality();
        let build_left = matches!(kind, JoinKind::Inner)
            && left_cardinality
                .zip(right_cardinality)
                .is_some_and(|(left, right)| left < right);
        let estimated_cardinality = left_cardinality
            .zip(right_cardinality)
            .map(|(left, right)| match kind {
                JoinKind::Inner => left.max(right),
                JoinKind::Left => left,
                JoinKind::Right => right,
                JoinKind::Full => left.saturating_add(right),
                JoinKind::Cross => left.saturating_mul(right),
            });
        let prepared_predicate = predicate.as_ref().and_then(|predicate| {
            ProjectedPredicate::compile_with_schema(predicate, &schema, &[])
                .ok()
                .flatten()
        });
        Self {
            left,
            right,
            kind,
            left_keys,
            right_keys,
            left_key_positions,
            right_key_positions,
            predicate,
            prepared_predicate,
            evaluator,
            left_nulls,
            right_nulls,
            schema,
            estimated_cardinality,
            build_left,
            work_mem_bytes,
            output: None,
            streaming_unique: None,
            output_spilled: SpillState::InMemory,
            right_input_spilled: SpillState::InMemory,
            hash_index_spilled: SpillState::InMemory,
        }
    }

    /// Construct a hash join while preparing supported residual predicates
    /// against its composite output schema. Parameters and constant LIKE
    /// patterns are folded exactly once before any candidate row is probed.
    #[allow(clippy::too_many_arguments)]
    pub fn try_new_with_work_mem_and_predicate(
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
        params: &[uqa_sql::SQLParam],
    ) -> ExecResult<Self> {
        let mut join = Self::new_with_work_mem_and_predicate(
            left,
            right,
            kind,
            left_keys,
            right_keys,
            predicate,
            evaluator,
            left_nulls,
            right_nulls,
            work_mem_bytes,
        );
        join.prepared_predicate = join
            .predicate
            .as_ref()
            .map(|predicate| {
                ProjectedPredicate::compile_with_schema(predicate, &join.schema, params)
            })
            .transpose()?
            .flatten();
        Ok(join)
    }

    pub fn output_has_spilled(&self) -> bool {
        self.output_spilled.is_spilled()
    }

    pub fn right_input_has_spilled(&self) -> bool {
        self.right_input_spilled.is_spilled()
    }

    pub fn hash_index_has_spilled(&self) -> bool {
        self.hash_index_spilled.is_spilled()
    }

    pub fn builds_left_input(&self) -> bool {
        self.build_left
    }

    fn rebuild_encoded_index(
        &self,
        rows: &mut HybridRowStore,
        expressions: &[ScalarExpr],
        positions: &[usize],
        budget_bytes: usize,
    ) -> ExecResult<HybridHashIndex> {
        let schema = rows.schema.clone();
        let mut index = HybridHashIndex::new(budget_bytes);
        for row_index in 0..rows.len() {
            let key = rows.with_row(row_index, |row| {
                self.key(expressions, Some(positions), row, &schema)
            })?;
            if let Some(key) = key {
                index.insert(key, row_index)?;
            }
        }
        Ok(index)
    }

    fn open_build_left(&mut self, state_budget: usize, output_budget: usize) -> ExecResult<()> {
        debug_assert!(matches!(self.kind, JoinKind::Inner));
        let left_budget = state_budget / 2;
        let hash_budget = state_budget.saturating_sub(left_budget);
        let left_schema = self.left.row_schema().clone();
        let mut left = HybridRowStore::new(left_schema, left_budget);
        let direct_positions = self
            .predicate
            .is_none()
            .then_some(())
            .and(self.left_key_positions.as_deref())
            .zip(self.right_key_positions.as_deref());
        let mut direct_index = direct_positions.map(|_| DirectHashIndex::new(hash_budget));
        let mut encoded_index = direct_index
            .is_none()
            .then(|| HybridHashIndex::new(hash_budget));
        self.left.open()?;
        while let Some(batch) = self.left.next()? {
            for row in batch.rows {
                let index = left.len();
                if let (Some(direct), Some((positions, _))) =
                    (direct_index.as_mut(), direct_positions)
                {
                    if let Some(hash) =
                        positional_key_hash(direct.hasher(), &batch.schema, &row, positions)?
                    {
                        direct.insert(hash, index)?;
                    }
                } else if let Some(key) = self.key(
                    &self.left_keys,
                    self.left_key_positions.as_deref(),
                    &row,
                    &batch.schema,
                )? {
                    encoded_index
                        .as_mut()
                        .ok_or_else(|| ExecError::Other("join hash index is missing".into()))?
                        .insert(key, index)?;
                }
                left.push(row)?;
            }
        }
        self.right_input_spilled = SpillState::InMemory;

        let mut output = SpillBuffer::new(output_budget);
        if left.len() == 0 {
            self.output = Some(output.drain()?);
            return Ok(());
        }

        let direct_is_unique = direct_index.as_ref().is_some_and(|direct| {
            direct_positions.is_some_and(|(positions, _)| {
                !left.has_spilled() && direct.keys_are_unique(&left, &left.schema, positions)
            })
        });
        if direct_is_unique {
            self.right.open()?;
            self.streaming_unique = Some(UniqueHashJoinState {
                build_rows: left,
                hash_index: UniqueHashIndex::Direct(
                    direct_index
                        .take()
                        .ok_or_else(|| ExecError::Other("direct join index is missing".into()))?,
                ),
                build_left: true,
            });
            return Ok(());
        }

        let mut left_by_key = match encoded_index {
            Some(index) => index,
            None => {
                let (positions, _) = direct_positions
                    .ok_or_else(|| ExecError::Other("direct join positions are missing".into()))?;
                self.rebuild_encoded_index(&mut left, &self.left_keys, positions, hash_budget)?
            }
        };
        self.hash_index_spilled = left_by_key.has_spilled().into();
        if self.predicate.is_none() && !left.has_spilled() && left_by_key.is_memory_unique() {
            self.right.open()?;
            self.streaming_unique = Some(UniqueHashJoinState {
                build_rows: left,
                hash_index: UniqueHashIndex::Encoded(left_by_key),
                build_left: true,
            });
            return Ok(());
        }
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);

        self.right.open()?;
        while let Some(batch) = self.right.next()? {
            for right_row in batch.rows {
                let Some(key) = self.key(
                    &self.right_keys,
                    self.right_key_positions.as_deref(),
                    &right_row,
                    &batch.schema,
                )?
                else {
                    continue;
                };
                if self.predicate.is_none() {
                    match left_by_key.memory_match_summary(&key) {
                        Some(MemoryMatchSummary::Absent) => continue,
                        Some(MemoryMatchSummary::Single(index)) => {
                            let merged = left.with_row(index, |left_row| {
                                Ok(PhysicalRow::concat_right_owned(left_row, right_row))
                            })?;
                            push_output_row(&mut output, &mut pending, &self.schema, merged)?;
                            continue;
                        }
                        Some(MemoryMatchSummary::Multiple) | None => {}
                    }
                }
                left_by_key.for_each_match(&key, &mut |index| {
                    left.with_row(index, |left_row| {
                        let merged = PhysicalRow::concat(left_row, &right_row);
                        if self.matches(&merged)? {
                            push_output_row(&mut output, &mut pending, &self.schema, merged)?;
                        }
                        Ok(())
                    })
                })?;
            }
        }
        if !pending.is_empty() {
            output.push(Batch::from_physical_rows(self.schema.clone(), pending))?;
        }
        self.output_spilled = output.has_spilled().into();
        self.output = Some(output.drain()?);
        Ok(())
    }

    fn key(
        &self,
        expressions: &[ScalarExpr],
        positions: Option<&[usize]>,
        row: &PhysicalRow,
        schema: &RowSchema,
    ) -> ExecResult<Option<EncodedKey>> {
        if let Some(positions) = positions {
            let view = schema.view(row);
            return encode_non_null_key(positions.iter().map(|position| view.value_at(*position)));
        }
        let mut values = SmallVec::<[Value; 4]>::with_capacity(expressions.len());
        for expression in expressions {
            let value = self.evaluator.evaluate_physical(expression, schema, row)?;
            if matches!(value, Value::Null) {
                return Ok(None);
            }
            values.push(value);
        }
        encode_non_null_key(values.iter().map(Some))
    }

    fn matches(&self, row: &PhysicalRow) -> ExecResult<bool> {
        if let Some(predicate) = self.prepared_predicate.as_ref() {
            return Ok(predicate.keep_row(&self.schema.view(row))?);
        }
        self.predicate.as_ref().map_or(Ok(true), |predicate| {
            Ok(truthy(&self.evaluator.evaluate_physical(
                predicate,
                &self.schema,
                row,
            )?))
        })
    }

    fn next_streaming_unique(
        &mut self,
        state: &mut UniqueHashJoinState,
    ) -> ExecResult<Option<Batch>> {
        loop {
            let next = if state.build_left {
                self.right.next()?
            } else {
                self.left.next()?
            };
            let Some(batch) = next else {
                return Ok(None);
            };
            let mut output = Vec::with_capacity(batch.rows.len());
            for probe_row in batch.rows {
                let index = match &state.hash_index {
                    UniqueHashIndex::Direct(index) => {
                        let (build_positions, probe_positions) = if state.build_left {
                            (
                                self.left_key_positions.as_deref(),
                                self.right_key_positions.as_deref(),
                            )
                        } else {
                            (
                                self.right_key_positions.as_deref(),
                                self.left_key_positions.as_deref(),
                            )
                        };
                        let (Some(build_positions), Some(probe_positions)) =
                            (build_positions, probe_positions)
                        else {
                            return Err(ExecError::Other(
                                "direct join key positions are missing".into(),
                            ));
                        };
                        direct_unique_match(
                            index,
                            &state.build_rows,
                            build_positions,
                            &batch.schema,
                            &probe_row,
                            probe_positions,
                        )?
                    }
                    UniqueHashIndex::Encoded(index) => {
                        let expressions = if state.build_left {
                            &self.right_keys
                        } else {
                            &self.left_keys
                        };
                        let positions = if state.build_left {
                            self.right_key_positions.as_deref()
                        } else {
                            self.left_key_positions.as_deref()
                        };
                        let Some(key) =
                            self.key(expressions, positions, &probe_row, &batch.schema)?
                        else {
                            continue;
                        };
                        match index.memory_match_summary(&key) {
                            Some(MemoryMatchSummary::Single(index)) => Some(index),
                            _ => None,
                        }
                    }
                };
                let Some(index) = index else { continue };
                let merged = if state.build_left {
                    state.build_rows.with_row(index, |build_row| {
                        Ok(PhysicalRow::concat_right_owned(build_row, probe_row))
                    })?
                } else {
                    state.build_rows.with_row(index, |build_row| {
                        Ok(PhysicalRow::concat_left_owned(probe_row, build_row))
                    })?
                };
                output.push(merged);
            }
            if !output.is_empty() {
                return Ok(Some(Batch::from_physical_rows(self.schema.clone(), output)));
            }
        }
    }
}

impl PhysicalOperator for HashJoin<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.estimated_cardinality
    }

    fn open(&mut self) -> ExecResult<()> {
        self.output = None;
        self.streaming_unique = None;
        self.output_spilled = SpillState::InMemory;
        self.right_input_spilled = SpillState::InMemory;
        self.hash_index_spilled = SpillState::InMemory;

        let state_budget = self.work_mem_bytes / 2;
        let output_budget = self.work_mem_bytes.saturating_sub(state_budget);
        if self.build_left {
            return self.open_build_left(state_budget, output_budget);
        }
        let right_budget = state_budget / 2;
        let hash_budget = state_budget.saturating_sub(right_budget);
        let right_schema = self.right.row_schema().clone();
        let mut right = HybridRowStore::new(right_schema, right_budget);
        let direct_positions = (matches!(self.kind, JoinKind::Inner) && self.predicate.is_none())
            .then_some(())
            .and(self.right_key_positions.as_deref())
            .zip(self.left_key_positions.as_deref());
        let mut direct_index = direct_positions.map(|_| DirectHashIndex::new(hash_budget));
        let mut encoded_index = direct_index
            .is_none()
            .then(|| HybridHashIndex::new(hash_budget));
        self.right.open()?;
        while let Some(batch) = self.right.next()? {
            for row in batch.rows {
                let index = right.len();
                if let (Some(direct), Some((positions, _))) =
                    (direct_index.as_mut(), direct_positions)
                {
                    if let Some(hash) =
                        positional_key_hash(direct.hasher(), &batch.schema, &row, positions)?
                    {
                        direct.insert(hash, index)?;
                    }
                } else if let Some(key) = self.key(
                    &self.right_keys,
                    self.right_key_positions.as_deref(),
                    &row,
                    &batch.schema,
                )? {
                    encoded_index
                        .as_mut()
                        .ok_or_else(|| ExecError::Other("join hash index is missing".into()))?
                        .insert(key, index)?;
                }
                right.push(row)?;
            }
        }
        self.right_input_spilled = right.has_spilled().into();

        if right.len() == 0 && matches!(self.kind, JoinKind::Inner) {
            let mut output = SpillBuffer::new(output_budget);
            self.output = Some(output.drain()?);
            return Ok(());
        }

        let direct_is_unique = direct_index.as_ref().is_some_and(|direct| {
            direct_positions.is_some_and(|(positions, _)| {
                !right.has_spilled() && direct.keys_are_unique(&right, &right.schema, positions)
            })
        });
        if direct_is_unique {
            self.left.open()?;
            self.streaming_unique = Some(UniqueHashJoinState {
                build_rows: right,
                hash_index: UniqueHashIndex::Direct(
                    direct_index
                        .take()
                        .ok_or_else(|| ExecError::Other("direct join index is missing".into()))?,
                ),
                build_left: false,
            });
            return Ok(());
        }

        let mut right_by_key = match encoded_index {
            Some(index) => index,
            None => {
                let (positions, _) = direct_positions
                    .ok_or_else(|| ExecError::Other("direct join positions are missing".into()))?;
                self.rebuild_encoded_index(&mut right, &self.right_keys, positions, hash_budget)?
            }
        };
        self.hash_index_spilled = right_by_key.has_spilled().into();
        if matches!(self.kind, JoinKind::Inner)
            && self.predicate.is_none()
            && !right.has_spilled()
            && right_by_key.is_memory_unique()
        {
            self.left.open()?;
            self.streaming_unique = Some(UniqueHashJoinState {
                build_rows: right,
                hash_index: UniqueHashIndex::Encoded(right_by_key),
                build_left: false,
            });
            return Ok(());
        }

        let mut matched_right = matches!(self.kind, JoinKind::Right | JoinKind::Full)
            .then(|| MatchFlags::new(right.len()))
            .transpose()?;
        let mut output = SpillBuffer::new(output_budget);
        let mut pending = Vec::with_capacity(crate::batch::DEFAULT_BATCH_SIZE);

        self.left.open()?;
        while let Some(batch) = self.left.next()? {
            for left_row in batch.rows {
                let mut matched_left = false;
                if let Some(key) = self.key(
                    &self.left_keys,
                    self.left_key_positions.as_deref(),
                    &left_row,
                    &batch.schema,
                )? {
                    if self.predicate.is_none() {
                        match right_by_key.memory_match_summary(&key) {
                            Some(MemoryMatchSummary::Absent) => {
                                if matches!(self.kind, JoinKind::Left | JoinKind::Full) {
                                    push_output_row(
                                        &mut output,
                                        &mut pending,
                                        &self.schema,
                                        PhysicalRow::concat_left_owned(left_row, &self.right_nulls),
                                    )?;
                                }
                                continue;
                            }
                            Some(MemoryMatchSummary::Single(index)) => {
                                let merged = right.with_row(index, |right_row| {
                                    Ok(PhysicalRow::concat_left_owned(left_row, right_row))
                                })?;
                                push_output_row(&mut output, &mut pending, &self.schema, merged)?;
                                if let Some(flags) = matched_right.as_mut() {
                                    flags.mark(index)?;
                                }
                                continue;
                            }
                            Some(MemoryMatchSummary::Multiple) | None => {}
                        }
                    }
                    right_by_key.for_each_match(&key, &mut |index| {
                        right.with_row(index, |right_row| {
                            let merged = PhysicalRow::concat(&left_row, right_row);
                            if self.matches(&merged)? {
                                push_output_row(&mut output, &mut pending, &self.schema, merged)?;
                                if let Some(flags) = matched_right.as_mut() {
                                    flags.mark(index)?;
                                }
                                matched_left = true;
                            }
                            Ok(())
                        })
                    })?;
                }
                if !matched_left && matches!(self.kind, JoinKind::Left | JoinKind::Full) {
                    push_output_row(
                        &mut output,
                        &mut pending,
                        &self.schema,
                        PhysicalRow::concat_left_owned(left_row, &self.right_nulls),
                    )?;
                }
            }
        }

        if matches!(self.kind, JoinKind::Right | JoinKind::Full) {
            let matched_right = matched_right.as_mut().ok_or_else(|| {
                ExecError::Other("right/full hash join has no match flags".into())
            })?;
            for index in 0..right.len() {
                if !matched_right.is_marked(index)? {
                    right.with_row(index, |right_row| {
                        push_output_row(
                            &mut output,
                            &mut pending,
                            &self.schema,
                            PhysicalRow::concat(&self.left_nulls, right_row),
                        )
                    })?;
                }
            }
        }
        if !pending.is_empty() {
            output.push(Batch::from_physical_rows(self.schema.clone(), pending))?;
        }
        self.output_spilled = output.has_spilled().into();
        self.output = Some(output.drain()?);
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if let Some(mut state) = self.streaming_unique.take() {
            let result = self.next_streaming_unique(&mut state);
            self.streaming_unique = Some(state);
            return result;
        }
        self.output
            .as_mut()
            .map_or(Ok(None), |output| output.next().transpose())
    }

    fn close(&mut self) -> ExecResult<()> {
        self.output = None;
        self.streaming_unique = None;
        let left = self.left.close();
        let right = self.right.close();
        crate::physical::with_cleanup(left, right, "close right hash-join input")
    }
}

#[cfg(test)]
mod tests;
