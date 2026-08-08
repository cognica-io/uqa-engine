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
//! streaming and preserves the first row for every key in child order.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use smallvec::{Array, SmallVec};
use tempfile::{Builder as TempBuilder, TempDir};
use uqa_core::{DecimalValue, TemporalValue, Value};
use uqa_sql::ResultRow;

use crate::{
    Batch, ExecError, ExecResult, PhysicalOperator, RowSchema, ScalarExpr,
    SharedExpressionEvaluator,
};

/// Default used by compatibility constructors. Engine callers should pass the
/// current session's `work_mem` through [`Distinct::all_with_work_mem`] or
/// [`Distinct::on_with_work_mem`].
pub const DEFAULT_DISTINCT_WORK_MEM_BYTES: usize = 64 * 1024 * 1024;

const DISK_BUCKETS: u64 = 64;
const COPY_BUFFER_BYTES: usize = 8 * 1024;
const MICROS_PER_DAY: i128 = 86_400_000_000;

pub(crate) type EncodedKey = SmallVec<[u8; 64]>;

/// Hash a borrowed positional SQL row in its canonical equality domain.
///
/// This streams encoded components straight into the caller's hasher, so it
/// does not allocate or construct an intermediate byte key. Hash collisions
/// remain possible; callers must verify complete [`Value`] equality before
/// reusing an existing row or group.
pub fn hash_canonical_row<'a, S: BuildHasher>(
    build_hasher: &S,
    values: impl ExactSizeIterator<Item = Option<&'a Value>>,
) -> ExecResult<u64> {
    let count = values.len();
    let mut hasher = build_hasher.build_hasher();
    {
        let mut output = HasherOutput(&mut hasher);
        encode_len(count, &mut output)?;
        for value in values {
            if let Some(value) = value {
                encode_value(value, &mut output)?;
            } else {
                output.push_byte(0);
            }
        }
    }
    Ok(hasher.finish())
}

/// Collision-safe in-memory set for positional SQL rows.
///
/// Probes consume borrowed values and stream their canonical representation
/// directly into the hash function. Only the first distinct row is copied
/// into the contiguous key arena; repeated build rows and every lookup avoid
/// both a positional `Vec<Value>` allocation and value cloning. Hash matches
/// always verify the complete SQL [`Value`] equality domain.
pub struct CanonicalRowHashSet {
    rows: Vec<SmallVec<[Value; 2]>>,
    index: HashMap<u64, SmallVec<[usize; 1]>, ahash::RandomState>,
}

impl CanonicalRowHashSet {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            index: HashMap::with_hasher(ahash::RandomState::new()),
        }
    }

    /// Insert a positional key assembled from borrowed values.
    /// Returns `true` only when this is the first SQL-equal key.
    pub fn insert_borrowed(&mut self, values: &[&Value]) -> ExecResult<bool> {
        let hash = hash_canonical_row(self.index.hasher(), values.iter().copied().map(Some))?;
        if self.matching_borrowed(hash, values) {
            return Ok(false);
        }

        let row = values
            .iter()
            .map(|value| (*value).clone())
            .collect::<SmallVec<[Value; 2]>>();
        let row_index = self.rows.len();
        self.rows.push(row);
        self.index.entry(hash).or_default().push(row_index);
        Ok(true)
    }

    /// Insert an already positional key without an intermediate borrowed-row
    /// carrier. Values are copied only for a previously unseen key.
    pub fn insert_values(&mut self, values: &[Value]) -> ExecResult<bool> {
        let hash = hash_canonical_row(self.index.hasher(), values.iter().map(Some))?;
        if self.matching_values(hash, values) {
            return Ok(false);
        }

        let row_index = self.rows.len();
        self.rows.push(values.iter().cloned().collect());
        self.index.entry(hash).or_default().push(row_index);
        Ok(true)
    }

    /// Probe with a composite row of borrowed values without allocating or
    /// copying the key.
    pub fn contains_borrowed(&self, values: &[&Value]) -> ExecResult<bool> {
        let hash = hash_canonical_row(self.index.hasher(), values.iter().copied().map(Some))?;
        Ok(self.matching_borrowed(hash, values))
    }

    /// Probe with an already positional value slice.
    pub fn contains_values(&self, values: &[Value]) -> ExecResult<bool> {
        let hash = hash_canonical_row(self.index.hasher(), values.iter().map(Some))?;
        Ok(self.matching_values(hash, values))
    }

    fn matching_borrowed(&self, hash: u64, values: &[&Value]) -> bool {
        self.index.get(&hash).is_some_and(|bucket| {
            bucket.iter().copied().any(|index| {
                let stored = &self.rows[index];
                stored.len() == values.len()
                    && stored
                        .iter()
                        .zip(values)
                        .all(|(stored, value)| stored == *value)
            })
        })
    }

    fn matching_values(&self, hash: u64, values: &[Value]) -> bool {
        self.index.get(&hash).is_some_and(|bucket| {
            bucket
                .iter()
                .copied()
                .any(|index| self.rows[index].as_slice() == values)
        })
    }
}

impl Default for CanonicalRowHashSet {
    fn default() -> Self {
        Self::new()
    }
}

/// Exact, byte-bounded row-key set that can outlive one physical operator.
///
/// Recursive fixpoint evaluation needs duplicate state to survive across
/// multiple executions of its recursive term. [`Distinct`] deliberately
/// resets its state on every `open`, so this small public carrier exposes the
/// same collision-safe memory-to-disk migration without coupling the engine to
/// the on-disk format.
pub struct ExactRowSet {
    seen: SeenKeySet,
}

impl ExactRowSet {
    pub fn new(work_mem_bytes: usize) -> Self {
        Self {
            seen: SeenKeySet::new(work_mem_bytes, None),
        }
    }

    pub fn with_spill_directory(work_mem_bytes: usize, directory: impl Into<PathBuf>) -> Self {
        Self {
            seen: SeenKeySet::new(work_mem_bytes, Some(directory.into())),
        }
    }

    /// Insert the positional values from `row` in `schema` order.
    /// Returns `true` only for the first exact occurrence.
    pub fn insert_row(&mut self, row: &ResultRow, schema: &[String]) -> ExecResult<bool> {
        self.seen.insert(row_key(row, schema)?)
    }

    pub fn contains_row(&mut self, row: &ResultRow, schema: &[String]) -> ExecResult<bool> {
        self.seen.contains(&row_key(row, schema)?)
    }

    /// Insert an already-positional SQL value key without constructing a
    /// named row. The binary encoding is the same collision-safe,
    /// cross-numeric representation used by physical DISTINCT.
    pub fn insert_values(&mut self, values: &[Value]) -> ExecResult<bool> {
        self.seen.insert(encode_key(values)?)
    }

    /// Probe an already-positional SQL value key without constructing a named
    /// row. Disk-backed sets perform an exact full-key comparison.
    pub fn contains_values(&mut self, values: &[Value]) -> ExecResult<bool> {
        self.seen.contains(&encode_key(values)?)
    }

    pub fn has_spilled(&self) -> bool {
        self.seen.has_spilled()
    }

    pub fn in_memory_key_bytes(&self) -> usize {
        self.seen.in_memory_bytes()
    }
}

fn row_key(row: &ResultRow, schema: &[String]) -> ExecResult<Vec<u8>> {
    let values = schema
        .iter()
        .map(|column| row.get(column).cloned().unwrap_or(Value::Null))
        .collect::<Vec<_>>();
    encode_key(&values)
}

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

    fn key(&self, row: &dyn uqa_sql::expr::RowLookup) -> ExecResult<Vec<Value>> {
        if let Some(keys) = self.keys.as_ref() {
            let evaluator = self.evaluator.as_ref().ok_or_else(|| {
                ExecError::Other("DISTINCT ON evaluator is not configured".into())
            })?;
            return keys
                .iter()
                .map(|expression| evaluator.evaluate(expression, row))
                .collect();
        }
        Ok(self
            .schema
            .columns()
            .iter()
            .enumerate()
            .map(|(index, _)| row.positional_column(index).cloned().unwrap_or(Value::Null))
            .collect())
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
            let mut rows = Vec::with_capacity(batch.rows.len());
            for row in batch.rows {
                let view = batch.schema.view(&row);
                let key = encode_key(&self.key(&view)?)?;
                if self.seen.insert(key)? {
                    rows.push(row);
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

pub(crate) struct SeenKeySet {
    memory: BTreeSet<Vec<u8>>,
    memory_bytes: usize,
    budget_bytes: usize,
    spill_directory: Option<PathBuf>,
    disk: Option<DiskKeySet>,
}

impl SeenKeySet {
    pub(crate) fn new(budget_bytes: usize, spill_directory: Option<PathBuf>) -> Self {
        Self {
            memory: BTreeSet::new(),
            memory_bytes: 0,
            budget_bytes,
            spill_directory,
            disk: None,
        }
    }

    pub(crate) fn insert(&mut self, key: Vec<u8>) -> ExecResult<bool> {
        if let Some(disk) = self.disk.as_mut() {
            return disk.insert(&key);
        }
        if self.memory.contains(&key) {
            return Ok(false);
        }

        let fits = self
            .memory_bytes
            .checked_add(key.len())
            .is_some_and(|bytes| bytes <= self.budget_bytes);
        if fits {
            self.memory_bytes += key.len();
            self.memory.insert(key);
            return Ok(true);
        }

        // Build the disk set off to the side. A create/migration/write error
        // leaves the original in-memory set intact and is returned to the
        // execution pipeline; no key is silently forgotten.
        let mut disk = DiskKeySet::new(self.spill_directory.as_deref())?;
        for existing in &self.memory {
            if !disk.insert(existing)? {
                return Err(distinct_error(
                    "duplicate key found while migrating DISTINCT state",
                ));
            }
        }
        let inserted = disk.insert(&key)?;
        self.memory.clear();
        self.memory_bytes = 0;
        self.disk = Some(disk);
        Ok(inserted)
    }

    pub(crate) fn contains(&mut self, key: &[u8]) -> ExecResult<bool> {
        match self.disk.as_mut() {
            Some(disk) => disk.contains(key),
            None => Ok(self.memory.contains(key)),
        }
    }

    fn has_spilled(&self) -> bool {
        self.disk.is_some()
    }

    fn in_memory_bytes(&self) -> usize {
        self.memory_bytes
    }

    fn spill_path(&self) -> Option<&Path> {
        self.disk.as_ref().map(|disk| disk.directory.path())
    }
}

/// Temporary bucketed exact set. Each record is `[u64 length][key bytes]`.
/// Bucket selection is only an accelerator: probes stream and compare the
/// complete record, making equality collision-free even if every hash collides.
struct DiskKeySet {
    directory: TempDir,
    buckets: BTreeMap<u8, File>,
}

impl DiskKeySet {
    fn new(parent: Option<&Path>) -> ExecResult<Self> {
        let mut builder = TempBuilder::new();
        builder.prefix("uqa-distinct-");
        let directory = parent
            .map_or_else(|| builder.tempdir(), |parent| builder.tempdir_in(parent))
            .map_err(|error| {
                distinct_error(format!(
                    "failed to create DISTINCT spill directory: {error}"
                ))
            })?;
        Ok(Self {
            directory,
            buckets: BTreeMap::new(),
        })
    }

    fn insert(&mut self, key: &[u8]) -> ExecResult<bool> {
        let bucket = u8::try_from(stable_hash(key) % DISK_BUCKETS)
            .map_err(|_| distinct_error("DISTINCT spill bucket exceeds u8"))?;
        if !self.buckets.contains_key(&bucket) {
            let path = self.directory.path().join(format!("bucket-{bucket:02x}"));
            let file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|error| {
                    distinct_error(format!(
                        "failed to create DISTINCT spill bucket {}: {error}",
                        path.display()
                    ))
                })?;
            self.buckets.insert(bucket, file);
        }
        let file = self
            .buckets
            .get_mut(&bucket)
            .ok_or_else(|| distinct_error("DISTINCT spill bucket registration failed"))?;

        file.seek(SeekFrom::Start(0)).map_err(|error| {
            distinct_error(format!("failed to seek DISTINCT spill bucket: {error}"))
        })?;
        while let Some(record_len) = read_record_len(file)? {
            let matches = compare_record(file, record_len, key)?;
            if matches {
                return Ok(false);
            }
        }

        let original_len = file.seek(SeekFrom::End(0)).map_err(|error| {
            distinct_error(format!("failed to seek DISTINCT spill bucket: {error}"))
        })?;
        let key_len = u64::try_from(key.len())
            .map_err(|_| distinct_error("DISTINCT key length exceeds the on-disk format"))?;
        let write_result = (|| {
            file.write_all(&key_len.to_le_bytes()).map_err(|error| {
                distinct_error(format!("failed to write DISTINCT key length: {error}"))
            })?;
            file.write_all(key).map_err(|error| {
                distinct_error(format!("failed to write DISTINCT key: {error}"))
            })?;
            file.flush().map_err(|error| {
                distinct_error(format!("failed to flush DISTINCT spill bucket: {error}"))
            })
        })();
        if let Err(error) = write_result {
            if let Err(rollback_error) = file.set_len(original_len) {
                return Err(distinct_error(format!(
                    "{error}; failed to roll back partial DISTINCT key: {rollback_error}"
                )));
            }
            return Err(error);
        }
        Ok(true)
    }

    fn contains(&mut self, key: &[u8]) -> ExecResult<bool> {
        let bucket = u8::try_from(stable_hash(key) % DISK_BUCKETS)
            .map_err(|_| distinct_error("DISTINCT spill bucket exceeds u8"))?;
        let Some(file) = self.buckets.get_mut(&bucket) else {
            return Ok(false);
        };
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            distinct_error(format!("failed to seek DISTINCT spill bucket: {error}"))
        })?;
        while let Some(record_len) = read_record_len(file)? {
            if compare_record(file, record_len, key)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn read_record_len(file: &mut File) -> ExecResult<Option<u64>> {
    let mut encoded = [0_u8; 8];
    match file.read(&mut encoded[..1]) {
        Ok(0) => return Ok(None),
        Ok(1) => {}
        Ok(count) => {
            return Err(distinct_error(format!(
                "invalid DISTINCT spill read count: requested 1 byte, received {count}"
            )));
        }
        Err(error) => {
            return Err(distinct_error(format!(
                "failed to read DISTINCT spill bucket: {error}"
            )));
        }
    }
    file.read_exact(&mut encoded[1..]).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            distinct_error("truncated DISTINCT spill key length")
        } else {
            distinct_error(format!("failed to read DISTINCT key length: {error}"))
        }
    })?;
    Ok(Some(u64::from_le_bytes(encoded)))
}

/// Compare one disk record without allocating a second key-sized buffer.
fn compare_record(file: &mut File, record_len: u64, key: &[u8]) -> ExecResult<bool> {
    let key_len = u64::try_from(key.len())
        .map_err(|_| distinct_error("DISTINCT key length exceeds the on-disk format"))?;
    let mut remaining = record_len;
    let mut offset = 0_usize;
    let mut matches = record_len == key_len;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    while remaining > 0 {
        let copy_buffer_bytes = u64::try_from(COPY_BUFFER_BYTES)
            .map_err(|_| distinct_error("DISTINCT copy buffer exceeds the on-disk length range"))?;
        let take = usize::try_from(remaining.min(copy_buffer_bytes)).map_err(|_| {
            distinct_error("DISTINCT spill key chunk exceeds the addressable memory range")
        })?;
        file.read_exact(&mut buffer[..take]).map_err(|error| {
            if error.kind() == ErrorKind::UnexpectedEof {
                distinct_error("truncated DISTINCT spill key")
            } else {
                distinct_error(format!("failed to read DISTINCT spill key: {error}"))
            }
        })?;
        if matches && buffer[..take] != key[offset..offset + take] {
            matches = false;
        }
        if matches {
            offset += take;
        }
        let consumed = u64::try_from(take)
            .map_err(|_| distinct_error("DISTINCT spill key chunk exceeds the length range"))?;
        remaining -= consumed;
    }
    Ok(matches)
}

fn stable_hash(bytes: &[u8]) -> u64 {
    // FNV-1a is deliberately fixed rather than using RandomState: spill files
    // are ephemeral, and equality always verifies the complete key anyway.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn distinct_error(message: impl Into<String>) -> ExecError {
    ExecError::Other(message.into())
}

/// Collision-free binary key encoding. Numeric values deliberately share one
/// canonical domain so `1`, `1.0`, `DECIMAL '1'`, and `TRUE` retain the same
/// equality behavior as UQA's SQL comparisons. Every structural value carries
/// lengths/counts, preventing concatenation and nested-container collisions.
pub(crate) fn encode_key(values: &[Value]) -> ExecResult<Vec<u8>> {
    let estimated_capacity = encoded_key_capacity(values.len())?;
    let mut output = Vec::with_capacity(estimated_capacity);
    encode_len(values.len(), &mut output)?;
    for value in values {
        encode_value(value, &mut output)?;
    }
    Ok(output)
}

/// Encode a join probe key directly from physical slots. Single- and
/// two-column numeric keys stay inline, and a NULL/missing component rejects
/// the SQL equality key without allocating or cloning a `Value`.
pub(crate) fn encode_non_null_key<'a>(
    values: impl ExactSizeIterator<Item = Option<&'a Value>>,
) -> ExecResult<Option<EncodedKey>> {
    let count = values.len();
    let mut output = EncodedKey::with_capacity(encoded_key_capacity(count)?);
    encode_len(count, &mut output)?;
    for value in values {
        let Some(value) = value else {
            return Ok(None);
        };
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        encode_value(value, &mut output)?;
    }
    Ok(Some(output))
}

fn encoded_key_capacity(values: usize) -> ExecResult<usize> {
    values
        .checked_mul(22)
        .and_then(|bytes| bytes.checked_add(8))
        .ok_or_else(|| distinct_error("DISTINCT key capacity overflow"))
}

trait KeyOutput {
    fn push_byte(&mut self, value: u8);
    fn extend_bytes(&mut self, values: &[u8]);
}

impl KeyOutput for Vec<u8> {
    fn push_byte(&mut self, value: u8) {
        self.push(value);
    }

    fn extend_bytes(&mut self, values: &[u8]) {
        self.extend_from_slice(values);
    }
}

impl<A: Array<Item = u8>> KeyOutput for SmallVec<A> {
    fn push_byte(&mut self, value: u8) {
        self.push(value);
    }

    fn extend_bytes(&mut self, values: &[u8]) {
        self.extend_from_slice(values);
    }
}

struct HasherOutput<'a, H: Hasher>(&'a mut H);

impl<H: Hasher> KeyOutput for HasherOutput<'_, H> {
    fn push_byte(&mut self, value: u8) {
        self.0.write_u8(value);
    }

    fn extend_bytes(&mut self, values: &[u8]) {
        self.0.write(values);
    }
}

fn encode_value(value: &Value, output: &mut impl KeyOutput) -> ExecResult<()> {
    match value {
        Value::Null => output.push_byte(0),
        Value::Bool(value) => encode_numeric_parts(i128::from(*value), 0, output),
        Value::Int(value) => encode_numeric_parts(i128::from(*value), 0, output),
        Value::Float(value) => encode_float_numeric(*value, output)?,
        Value::Decimal(value) => encode_decimal_numeric(value, output),
        Value::Str(value) => {
            output.push_byte(2);
            encode_bytes(value.as_bytes(), output)?;
        }
        Value::FixedChar(value) => {
            output.push_byte(7);
            encode_bytes(value.trim_end_matches(' ').as_bytes(), output)?;
        }
        Value::Bytes(value) => {
            output.push_byte(3);
            encode_bytes(value, output)?;
        }
        Value::Temporal(value) => encode_temporal(value, output),
        Value::List(values) => {
            output.push_byte(5);
            encode_len(values.len(), output)?;
            for value in values {
                encode_value(value, output)?;
            }
        }
        Value::Map(values) => {
            output.push_byte(6);
            encode_len(values.len(), output)?;
            for (name, value) in values {
                encode_bytes(name.as_bytes(), output)?;
                encode_value(value, output)?;
            }
        }
    }
    Ok(())
}

fn encode_decimal_numeric(value: &DecimalValue, output: &mut impl KeyOutput) {
    let (coefficient, scale) = value.canonical_parts();
    encode_numeric_parts(coefficient, scale, output);
}

fn encode_numeric_parts(coefficient: i128, scale: u32, output: &mut impl KeyOutput) {
    output.extend_bytes(&[1, 0]);
    output.extend_bytes(&coefficient.to_be_bytes());
    output.extend_bytes(&scale.to_be_bytes());
}

fn encode_float_numeric(value: f64, output: &mut impl KeyOutput) -> ExecResult<()> {
    if value.is_nan() {
        // PostgreSQL groups all NaN values together for DISTINCT.
        output.extend_bytes(&[1, 1]);
    } else if value == f64::NEG_INFINITY {
        output.extend_bytes(&[1, 2]);
    } else if value == f64::INFINITY {
        output.extend_bytes(&[1, 3]);
    } else if let Some(decimal) = DecimalValue::from_f64_lossy(value) {
        encode_decimal_numeric(&decimal, output);
    } else {
        // A finite f64 outside rust_decimal's exponent range still needs a
        // lossless representation. Normalize signed zero before storing bits.
        output.extend_bytes(&[1, 4]);
        let normalized = if value == 0.0 { 0.0 } else { value };
        output.extend_bytes(&normalized.to_bits().to_be_bytes());
    }
    Ok(())
}

fn encode_temporal(value: &TemporalValue, output: &mut impl KeyOutput) {
    output.push_byte(4);
    match value {
        TemporalValue::Date { days } => {
            output.push_byte(0);
            output.extend_bytes(&days.to_be_bytes());
        }
        TemporalValue::Time { micros } => {
            output.push_byte(1);
            let normalized = i128::from(*micros).rem_euclid(MICROS_PER_DAY);
            output.extend_bytes(&normalized.to_be_bytes());
        }
        TemporalValue::TimeTz {
            micros,
            offset_minutes,
        } => {
            output.push_byte(2);
            let normalized = (i128::from(*micros) - i128::from(*offset_minutes) * 60_000_000)
                .rem_euclid(MICROS_PER_DAY);
            output.extend_bytes(&normalized.to_be_bytes());
        }
        TemporalValue::Timestamp { micros } => {
            output.push_byte(3);
            output.extend_bytes(&micros.to_be_bytes());
        }
        TemporalValue::TimestampTz { micros } => {
            output.push_byte(4);
            output.extend_bytes(&micros.to_be_bytes());
        }
        TemporalValue::Interval {
            months,
            days,
            micros,
        } => {
            output.push_byte(5);
            let normalized = (i128::from(*months) * 30 + i128::from(*days)) * MICROS_PER_DAY
                + i128::from(*micros);
            output.extend_bytes(&normalized.to_be_bytes());
        }
    }
}

fn encode_bytes(bytes: &[u8], output: &mut impl KeyOutput) -> ExecResult<()> {
    encode_len(bytes.len(), output)?;
    output.extend_bytes(bytes);
    Ok(())
}

fn encode_len(length: usize, output: &mut impl KeyOutput) -> ExecResult<()> {
    let length = u64::try_from(length)
        .map_err(|_| distinct_error("DISTINCT key component exceeds the binary format"))?;
    output.extend_bytes(&length.to_be_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::NamedTempFile;

    use super::*;
    use crate::physical::run_to_rows;
    use crate::scan::TableScan;
    use crate::{ExpressionEvaluator, ScalarEvalContext};

    fn row(a: i64, b: i64) -> ResultRow {
        [("a".into(), Value::Int(a)), ("b".into(), Value::Int(b))]
            .into_iter()
            .collect()
    }

    fn value_row(value: Value) -> ResultRow {
        [("v".into(), value)].into_iter().collect()
    }

    struct Evaluator;

    impl ExpressionEvaluator for Evaluator {
        fn evaluate(
            &self,
            expression: &ScalarExpr,
            row: &dyn uqa_sql::expr::RowLookup,
        ) -> ExecResult<Value> {
            Ok(crate::eval_scalar(
                expression,
                &ScalarEvalContext::from_row_lookup(row, &[]),
            )?)
        }
    }

    #[test]
    fn all_columns_and_distinct_on_preserve_the_first_row() {
        let rows = vec![row(1, 10), row(1, 10), row(1, 11), row(2, 20)];
        let scan = TableScan::from_rows(vec!["a".into(), "b".into()], rows.clone());
        let mut all = Distinct::all_with_work_mem(Box::new(scan), 1);
        let (_, all_rows) = run_to_rows(&mut all).unwrap();
        assert_eq!(all_rows, vec![row(1, 10), row(1, 11), row(2, 20)]);

        let scan = TableScan::from_rows(vec!["a".into(), "b".into()], rows);
        let mut on = Distinct::on_with_work_mem(
            Box::new(scan),
            vec![ScalarExpr::Column("a".into())],
            Arc::new(Evaluator),
            1,
        );
        let (_, on_rows) = run_to_rows(&mut on).unwrap();
        assert_eq!(on_rows, vec![row(1, 10), row(2, 20)]);
    }

    #[test]
    fn tiny_budget_migrates_to_disk_and_never_retains_key_bytes() {
        let rows: Vec<_> = (0..50)
            .flat_map(|value| [value_row(Value::Int(value)), value_row(Value::Int(value))])
            .collect();
        let scan = TableScan::from_rows(vec!["v".into()], rows);
        let mut distinct = Distinct::all_with_work_mem(Box::new(scan), 1);
        distinct.open().unwrap();
        let output = distinct.next().unwrap().unwrap();
        assert_eq!(output.rows.len(), 50);
        assert!(distinct.has_spilled());
        assert_eq!(distinct.in_memory_key_bytes(), 0);
        assert!(distinct.next().unwrap().is_none());
        distinct.close().unwrap();
    }

    #[test]
    fn exact_row_set_persists_disk_backed_state_across_fixpoint_phases() {
        let schema = vec!["a".into(), "b".into()];
        let mut seen = ExactRowSet::new(1);
        for value in 0..100 {
            assert!(seen.insert_row(&row(value, value + 1), &schema).unwrap());
        }
        assert!(seen.has_spilled());
        assert_eq!(seen.in_memory_key_bytes(), 0);
        for value in 0..100 {
            assert!(seen.contains_row(&row(value, value + 1), &schema).unwrap());
            assert!(!seen.insert_row(&row(value, value + 1), &schema).unwrap());
        }
        assert!(!seen.contains_row(&row(101, 102), &schema).unwrap());
    }

    #[test]
    fn binary_keys_cover_every_value_variant_without_structural_collisions() {
        let one = DecimalValue::parse("1.000").unwrap();
        let mut nested_map = BTreeMap::new();
        nested_map.insert("x".into(), Value::Float(1.0));
        let values = vec![
            Value::Null,
            Value::Bool(true),
            Value::Int(1),
            Value::Float(1.0),
            Value::Decimal(one),
            Value::Float(f64::NAN),
            Value::Float(f64::from_bits(0x7ff8_0000_0000_0001)),
            Value::Float(f64::NEG_INFINITY),
            Value::Float(f64::INFINITY),
            Value::Str("a\0b".into()),
            Value::Bytes(vec![b'a', 0, b'b']),
            Value::Temporal(TemporalValue::Date { days: 1 }),
            Value::Temporal(TemporalValue::Time {
                micros: MICROS_PER_DAY as i64 + 7,
            }),
            Value::Temporal(TemporalValue::Time { micros: 7 }),
            Value::Temporal(TemporalValue::TimeTz {
                micros: 3_600_000_000,
                offset_minutes: 60,
            }),
            Value::Temporal(TemporalValue::TimeTz {
                micros: 0,
                offset_minutes: 0,
            }),
            Value::Temporal(TemporalValue::Timestamp { micros: 9 }),
            Value::Temporal(TemporalValue::TimestampTz { micros: 9 }),
            Value::Temporal(TemporalValue::Interval {
                months: 1,
                days: 0,
                micros: 0,
            }),
            Value::Temporal(TemporalValue::Interval {
                months: 0,
                days: 30,
                micros: 0,
            }),
            Value::List(vec![Value::Int(1), Value::Str("x".into())]),
            Value::List(vec![Value::Float(1.0), Value::Str("x".into())]),
            Value::Map(nested_map),
        ];
        let rows: Vec<_> = values
            .iter()
            .cloned()
            .chain(values.iter().cloned())
            .map(value_row)
            .collect();
        let scan = TableScan::from_rows(vec!["v".into()], rows);
        let mut distinct = Distinct::all_with_work_mem(Box::new(scan), 0);
        let (_, output) = run_to_rows(&mut distinct).unwrap();

        // true/int/float/decimal share one numeric key; NaN payloads share one;
        // normalized time/time-tz/interval pairs and nested numeric values do
        // likewise. The string and byte representations stay distinct.
        assert_eq!(output.len(), 15);
        assert_eq!(output[1], value_row(Value::Bool(true)));
        assert!(matches!(output[2].get("v"), Some(Value::Float(v)) if v.is_nan()));
        assert_eq!(output[5], value_row(Value::Str("a\0b".into())));
        assert_eq!(output[6], value_row(Value::Bytes(vec![b'a', 0, b'b'])));
    }

    #[test]
    fn physical_numeric_join_key_stays_inline_and_matches_distinct_encoding() {
        let value = Value::Int(42);
        let key = encode_non_null_key(std::iter::once(Some(&value)))
            .unwrap()
            .unwrap();
        assert!(!key.spilled());
        assert_eq!(
            key.as_slice(),
            encode_key(std::slice::from_ref(&value)).unwrap()
        );

        let null = Value::Null;
        assert!(encode_non_null_key(std::iter::once(Some(&null)))
            .unwrap()
            .is_none());
    }

    #[test]
    fn canonical_row_hash_streams_borrowed_composites_with_sql_equality() {
        let integer = Value::Int(1);
        let decimal = Value::Decimal(DecimalValue::parse("1.000").unwrap());
        let text = Value::Str("group".into());
        let hash_state = ahash::RandomState::new();
        assert_eq!(
            hash_canonical_row(&hash_state, [Some(&integer), Some(&text)].into_iter()).unwrap(),
            hash_canonical_row(&hash_state, [Some(&decimal), Some(&text)].into_iter()).unwrap()
        );

        let null = Value::Null;
        assert_eq!(
            hash_canonical_row(&hash_state, std::iter::once(None)).unwrap(),
            hash_canonical_row(&hash_state, std::iter::once(Some(&null))).unwrap()
        );
    }

    #[test]
    fn canonical_row_hash_set_copies_only_new_keys_and_probes_borrowed() {
        let one = Value::Int(1);
        let two = Value::Int(2);
        let decimal_one = Value::Decimal(DecimalValue::parse("1.000").unwrap());
        let mut rows = CanonicalRowHashSet::new();

        assert!(rows.insert_borrowed(&[&one, &two]).unwrap());
        assert!(!rows.insert_borrowed(&[&decimal_one, &two]).unwrap());
        assert!(rows.contains_borrowed(&[&decimal_one, &two]).unwrap());
        assert!(!rows.contains_borrowed(&[&two, &one]).unwrap());
        assert_eq!(rows.rows.len(), 1);
        assert!(!rows.rows[0].spilled());
    }

    #[test]
    fn temporary_directory_is_removed_on_drop() {
        let parent = tempfile::tempdir().unwrap();
        let scan = TableScan::from_rows(vec!["v".into()], vec![value_row(Value::Int(1))]);
        let mut distinct =
            Distinct::all_with_work_mem(Box::new(scan), 0).with_spill_directory(parent.path());
        distinct.open().unwrap();
        distinct.next().unwrap();
        let spill_path = distinct.spill_path().unwrap().to_path_buf();
        assert!(spill_path.exists());
        drop(distinct);
        assert!(!spill_path.exists());
    }

    #[test]
    fn spill_creation_failure_is_returned() {
        let not_a_directory = NamedTempFile::new().unwrap();
        let scan = TableScan::from_rows(vec!["v".into()], vec![value_row(Value::Int(1))]);
        let mut distinct = Distinct::all_with_work_mem(Box::new(scan), 0)
            .with_spill_directory(not_a_directory.path());
        let error = run_to_rows(&mut distinct).unwrap_err();
        assert!(error
            .to_string()
            .contains("failed to create DISTINCT spill directory"));
    }

    #[test]
    fn truncated_disk_record_is_reported() {
        let first = encode_key(&[Value::Int(1)]).unwrap();
        let bucket = stable_hash(&first) % DISK_BUCKETS;
        let second = (2..10_000)
            .map(|value| encode_key(&[Value::Int(value)]).unwrap())
            .find(|key| stable_hash(key) % DISK_BUCKETS == bucket)
            .unwrap();
        let mut set = SeenKeySet::new(0, None);
        assert!(set.insert(first).unwrap());
        let disk = set.disk.as_mut().unwrap();
        let file = disk.buckets.get_mut(&(bucket as u8)).unwrap();
        file.set_len(4).unwrap();
        let error = set.insert(second).unwrap_err();
        assert!(error
            .to_string()
            .contains("truncated DISTINCT spill key length"));
    }

    struct FailingEvaluator;

    impl ExpressionEvaluator for FailingEvaluator {
        fn evaluate(
            &self,
            _expression: &ScalarExpr,
            _row: &dyn uqa_sql::expr::RowLookup,
        ) -> ExecResult<Value> {
            Err(ExecError::Other("intentional evaluator failure".into()))
        }
    }

    #[test]
    fn evaluator_errors_are_propagated() {
        let scan = TableScan::from_rows(vec!["v".into()], vec![value_row(Value::Int(1))]);
        let mut distinct = Distinct::on_with_work_mem(
            Box::new(scan),
            vec![ScalarExpr::Column("v".into())],
            Arc::new(FailingEvaluator),
            0,
        );
        let error = run_to_rows(&mut distinct).unwrap_err();
        assert!(error.to_string().contains("intentional evaluator failure"));
    }
}
