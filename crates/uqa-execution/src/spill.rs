//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Disk-backed spill buffer for blocking operators (`Sort`,
//! `HashAggregate`, `Window`).
//!
//! The budget is measured in the exact number of bytes each batch occupies in
//! the spill encoding. [`SpillBuffer::push`] automatically flushes before a
//! successful push could leave more encoded bytes in memory than the budget.
//! Draining restores spilled batches first and then any in-memory tail,
//! preserving input order. The temporary file is removed when the buffer (or
//! its active drain iterator) is dropped.

use std::fs::File;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::batch::{Batch, OwnedPhysicalRow, PhysicalRow, RowSchema};
use crate::physical::ExecResult;
use tempfile::NamedTempFile;

mod format;
mod indexed;

use format::{
    append_batches, decode_batch, encoded_batch_overhead_size, encoded_batch_size,
    encoded_physical_row_record_size, open_spill_reader, read_bounded_spill_record, spill_error,
};
pub use indexed::IndexedSpill;

const SPILL_MAGIC: &[u8] = b"UQA-SPILL\x01\n";

/// Incremental exact size of one not-yet-encoded batch. When the first row with lock origins arrives, the binary format adds an empty origin-count field to every preceding origin-free row, so accounting must update those already-buffered rows as well as the new record.
#[derive(Clone, Copy)]
pub(crate) struct EncodedBatchSizer {
    physical_width: usize,
    bytes: usize,
    origin_free_rows: usize,
    has_lock_origins: bool,
}

impl EncodedBatchSizer {
    pub(crate) fn new(schema: &RowSchema) -> ExecResult<Self> {
        Ok(Self {
            physical_width: schema.physical_width(),
            bytes: encoded_batch_overhead_size(schema)?,
            origin_free_rows: 0,
            has_lock_origins: false,
        })
    }

    pub(crate) fn append(&mut self, row: &PhysicalRow) -> ExecResult<()> {
        let mut additional = encoded_physical_row_record_size(row, self.physical_width)?;
        if row.lock_origins().is_empty() {
            self.origin_free_rows = self.origin_free_rows.checked_add(1).ok_or_else(|| {
                spill_error("incremental spill batch origin-free row count overflow")
            })?;
            if self.has_lock_origins {
                additional = additional.checked_add(8).ok_or_else(|| {
                    spill_error("incremental spill batch lock-origin size overflow")
                })?;
            }
        } else if !self.has_lock_origins {
            let preceding_metadata = self.origin_free_rows.checked_mul(8).ok_or_else(|| {
                spill_error("incremental spill batch lock-origin metadata overflow")
            })?;
            additional = additional
                .checked_add(preceding_metadata)
                .ok_or_else(|| spill_error("incremental spill batch lock-origin size overflow"))?;
            self.has_lock_origins = true;
        }
        self.bytes = self
            .bytes
            .checked_add(additional)
            .ok_or_else(|| spill_error("incremental spill batch size overflow"))?;
        Ok(())
    }

    pub(crate) fn bytes(self) -> usize {
        self.bytes
    }
}

/// Append-only batch buffer with an encoded-byte memory budget.
///
/// The budget is exact for the serialized representation and does not claim to
/// be the Rust allocator's resident-byte accounting. At most one incoming or
/// decoded batch can itself be larger than the budget; successful pushes do not
/// retain such an oversized batch in memory.
pub struct SpillBuffer {
    schema: Option<RowSchema>,
    batches: Vec<Batch>,
    rows: usize,
    in_memory_rows: usize,
    in_memory_bytes: usize,
    max_in_memory_record_bytes: usize,
    /// Encoded-byte budget. Set to `usize::MAX` to disable spilling.
    budget_bytes: usize,
    spill_directory: Option<PathBuf>,
    spill_file: Option<NamedTempFile>,
    spilled_batches: usize,
    spilled_rows: usize,
    spilled_bytes: usize,
    max_spilled_record_bytes: usize,
}

impl SpillBuffer {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            schema: None,
            batches: Vec::new(),
            rows: 0,
            in_memory_rows: 0,
            in_memory_bytes: 0,
            max_in_memory_record_bytes: 0,
            budget_bytes,
            spill_directory: None,
            spill_file: None,
            spilled_batches: 0,
            spilled_rows: 0,
            spilled_bytes: 0,
            max_spilled_record_bytes: 0,
        }
    }

    /// Create a buffer whose temporary spill file will be placed in `directory`.
    ///
    /// File creation is deferred until the first spill. This is primarily useful
    /// when an engine has a dedicated temporary-data volume.
    pub fn new_in(budget_bytes: usize, directory: impl Into<PathBuf>) -> Self {
        let mut buffer = Self::new(budget_bytes);
        buffer.spill_directory = Some(directory.into());
        buffer
    }

    pub fn unbounded() -> Self {
        Self::new(usize::MAX)
    }

    /// Append a batch, spilling automatically when required by the byte budget.
    ///
    /// Returns `true` if this push wrote one or more batches to disk. If disk
    /// creation, encoding, or writing fails, the new batch and all earlier
    /// batches remain owned by the buffer and the error is returned.
    pub fn push(&mut self, batch: Batch) -> ExecResult<bool> {
        if let Some(schema) = self.schema.as_ref() {
            if schema != &batch.schema {
                return Err(spill_error(format!(
                    "spill buffer schema mismatch: expected {:?}, got {:?}",
                    schema.columns(),
                    batch.schema.columns()
                )));
            }
        } else {
            self.schema = Some(batch.schema.clone());
        }
        let batch_rows = batch.rows.len();
        let next_rows = self
            .rows
            .checked_add(batch_rows)
            .ok_or_else(|| spill_error("spill buffer row count overflow"))?;
        let batch_bytes = match Self::encoded_size(&batch) {
            Ok(bytes) => bytes,
            Err(error) => {
                // Preserve ownership even when an exotic value or an encoded
                // size overflow prevents budget accounting. The failed
                // operator will abort, but no row silently disappears.
                self.retain_batch(batch, usize::MAX);
                return Err(error);
            }
        };
        let would_exceed = self
            .in_memory_bytes
            .checked_add(batch_bytes)
            .is_none_or(|bytes| bytes > self.budget_bytes);

        let mut spilled = false;
        if would_exceed && !self.batches.is_empty() {
            if let Err(error) = self.spill_pending() {
                self.retain_batch(batch, batch_bytes);
                return Err(error);
            }
            spilled = true;
        }

        let next_in_memory_rows = self
            .in_memory_rows
            .checked_add(batch_rows)
            .ok_or_else(|| spill_error("spill buffer in-memory row count overflow"))?;
        let next_in_memory_bytes = self
            .in_memory_bytes
            .checked_add(batch_bytes)
            .ok_or_else(|| spill_error("spill buffer in-memory byte count overflow"))?;
        self.rows = next_rows;
        self.in_memory_rows = next_in_memory_rows;
        self.in_memory_bytes = next_in_memory_bytes;
        self.max_in_memory_record_bytes = self.max_in_memory_record_bytes.max(batch_bytes);
        self.batches.push(batch);

        // A single encoded batch may exceed work_mem. It must pass through
        // memory once, but a successful push never retains it there.
        if self.in_memory_bytes > self.budget_bytes {
            self.spill_pending()?;
            spilled = true;
        }
        Ok(spilled)
    }

    /// Exact byte count used for budget accounting, including the record
    /// length prefix written to disk.
    pub fn encoded_size(batch: &Batch) -> ExecResult<usize> {
        encoded_batch_size(batch)
    }

    /// Total buffered rows, including rows already written to disk.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Rows currently retained in memory.
    pub fn in_memory_rows(&self) -> usize {
        self.in_memory_rows
    }

    /// Exact encoded bytes currently retained in memory.
    pub fn in_memory_bytes(&self) -> usize {
        self.in_memory_bytes
    }

    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    pub fn over_budget(&self) -> bool {
        self.in_memory_bytes > self.budget_bytes
    }

    pub fn has_spilled(&self) -> bool {
        self.spill_file.is_some()
    }

    pub fn spilled_rows(&self) -> usize {
        self.spilled_rows
    }

    pub fn spilled_batches(&self) -> usize {
        self.spilled_batches
    }

    pub fn spilled_bytes(&self) -> usize {
        self.spilled_bytes
    }

    /// Path of the live spill file, if one has been created.
    ///
    /// The path is diagnostic only and becomes invalid as soon as the buffer or
    /// the drain iterator that owns the file is dropped.
    pub fn spill_path(&self) -> Option<&Path> {
        self.spill_file.as_ref().map(NamedTempFile::path)
    }

    /// Flush all pending in-memory batches when the byte budget is exceeded.
    ///
    /// Returns `true` when batches were written. A failed append is rolled back
    /// to the previous file length and the pending batches remain in memory, so
    /// callers never observe a silent partial spill.
    pub fn spill_if_over_budget(&mut self) -> ExecResult<bool> {
        if !self.over_budget() || self.batches.is_empty() {
            return Ok(false);
        }
        self.spill_pending()
    }

    /// Force all pending in-memory batches to disk regardless of the budget.
    ///
    /// This is useful at a blocking-operator phase boundary. It returns `false`
    /// when there is nothing pending.
    pub fn spill_pending(&mut self) -> ExecResult<bool> {
        if self.batches.is_empty() {
            return Ok(false);
        }

        // Reject metadata overflow before writing. An error after a successful
        // append would leave a retry able to duplicate records while the
        // published counters disagreed with the file.
        let next_spilled_batches = self
            .spilled_batches
            .checked_add(self.batches.len())
            .ok_or_else(|| spill_error("spill batch count overflow"))?;
        let next_spilled_rows = self
            .spilled_rows
            .checked_add(self.in_memory_rows)
            .ok_or_else(|| spill_error("spill row count overflow"))?;
        let next_spilled_bytes = self
            .spilled_bytes
            .checked_add(self.in_memory_bytes)
            .ok_or_else(|| spill_error("spill byte count overflow"))?;
        let next_max_spilled_record_bytes = self
            .max_spilled_record_bytes
            .max(self.max_in_memory_record_bytes);

        if let Some(file) = self.spill_file.as_mut() {
            append_batches(file.as_file_mut(), &self.batches)?;
        } else {
            let mut file = self.create_spill_file()?;
            append_batches(file.as_file_mut(), &self.batches)?;
            self.spill_file = Some(file);
        }

        self.spilled_batches = next_spilled_batches;
        self.spilled_rows = next_spilled_rows;
        self.spilled_bytes = next_spilled_bytes;
        self.max_spilled_record_bytes = next_max_spilled_record_bytes;
        self.batches.clear();
        self.in_memory_rows = 0;
        self.in_memory_bytes = 0;
        self.max_in_memory_record_bytes = 0;
        Ok(true)
    }

    /// Open a repeatable streaming reader without consuming this buffer.
    ///
    /// Spilled batches are decoded one at a time. The in-memory tail is cloned
    /// one batch at a time only when the reader reaches it.
    pub fn reader(&self) -> ExecResult<SpillReader<'_>> {
        let reader = self
            .spill_file
            .as_ref()
            .map(open_spill_reader)
            .transpose()?;
        let disk_finished = reader.is_none();
        Ok(SpillReader {
            reader,
            memory: self.batches.iter(),
            disk_finished,
            failed: false,
            max_record_bytes: self.max_spilled_record_bytes,
            expected_schema: self.schema.clone(),
        })
    }

    /// Open a repeatable physical-row stream without collecting all batches.
    pub fn read_rows(&self) -> ExecResult<SpillRows<SpillReader<'_>>> {
        self.reader().map(SpillRows::new)
    }

    /// Drain buffered batches in their original input order.
    ///
    /// The returned iterator owns the temporary file. Each disk read or decode
    /// failure is returned as a [`crate::physical::ExecError`], and dropping the
    /// iterator early still removes the temporary file.
    pub fn drain(&mut self) -> ExecResult<SpillDrain> {
        let reader = self
            .spill_file
            .as_ref()
            .map(open_spill_reader)
            .transpose()?;
        let spill_file = self.spill_file.take();
        let memory = std::mem::take(&mut self.batches).into_iter();
        let expected_schema = self.schema.take();

        self.rows = 0;
        self.in_memory_rows = 0;
        self.in_memory_bytes = 0;
        self.max_in_memory_record_bytes = 0;
        self.spilled_batches = 0;
        self.spilled_rows = 0;
        self.spilled_bytes = 0;
        let max_record_bytes = std::mem::take(&mut self.max_spilled_record_bytes);

        let disk_finished = reader.is_none();
        Ok(SpillDrain {
            reader,
            spill_file,
            memory,
            disk_finished,
            failed: false,
            max_record_bytes,
            expected_schema,
        })
    }

    /// Drain and materialize every restored batch.
    pub fn drain_all(&mut self) -> ExecResult<Vec<Batch>> {
        self.drain()?.collect()
    }

    /// Consume the buffer as a physical-row stream without collecting batches.
    pub fn drain_rows(&mut self) -> ExecResult<SpillRows<SpillDrain>> {
        self.drain().map(SpillRows::new)
    }

    /// Discard all buffered data and remove any spill file.
    pub fn clear(&mut self) {
        self.schema = None;
        self.batches.clear();
        self.spill_file = None;
        self.rows = 0;
        self.in_memory_rows = 0;
        self.in_memory_bytes = 0;
        self.max_in_memory_record_bytes = 0;
        self.spilled_batches = 0;
        self.spilled_rows = 0;
        self.spilled_bytes = 0;
        self.max_spilled_record_bytes = 0;
    }

    /// Seal this buffer as an immutable, cheaply cloneable materialization.
    /// Batches that fit within the configured byte budget remain in memory;
    /// once spilling has started, every pending batch is flushed and readers
    /// reopen the file independently. Both forms support repeatable scans
    /// without collecting the complete input again.
    pub fn into_shared(mut self, schema: impl Into<RowSchema>) -> ExecResult<SharedSpill> {
        let schema = schema.into();
        if let Some(actual) = self.schema.as_ref() {
            if actual != &schema {
                return Err(spill_error(format!(
                    "shared spill schema mismatch: expected {:?}, got {:?}",
                    schema.columns(),
                    actual.columns()
                )));
            }
        }
        let rows = self.rows;
        let storage = if self.spill_file.is_none() {
            let batches = std::mem::take(&mut self.batches);
            SharedSpillStorage::Memory(batches)
        } else {
            self.spill_pending()?;
            SharedSpillStorage::Disk(
                self.spill_file
                    .take()
                    .expect("spill file exists after flushing shared materialization"),
            )
        };
        Ok(SharedSpill {
            inner: Arc::new(SharedSpillInner {
                storage,
                schema,
                rows,
                max_record_bytes: self.max_spilled_record_bytes,
            }),
        })
    }

    fn create_spill_file(&self) -> ExecResult<NamedTempFile> {
        let mut file = match &self.spill_directory {
            Some(directory) => NamedTempFile::new_in(directory).map_err(|error| {
                spill_error(format!(
                    "failed to create spill file in {}: {error}",
                    directory.display()
                ))
            })?,
            None => NamedTempFile::new()
                .map_err(|error| spill_error(format!("failed to create spill file: {error}")))?,
        };
        file.as_file_mut()
            .write_all(SPILL_MAGIC)
            .map_err(|error| spill_error(format!("failed to initialize spill file: {error}")))?;
        file.as_file_mut()
            .flush()
            .map_err(|error| spill_error(format!("failed to flush spill header: {error}")))?;
        Ok(file)
    }

    fn retain_batch(&mut self, batch: Batch, encoded_bytes: usize) {
        self.rows = self.rows.saturating_add(batch.rows.len());
        self.in_memory_rows = self.in_memory_rows.saturating_add(batch.rows.len());
        self.in_memory_bytes = self.in_memory_bytes.saturating_add(encoded_bytes);
        self.max_in_memory_record_bytes = self.max_in_memory_record_bytes.max(encoded_bytes);
        self.batches.push(batch);
    }
}

enum SharedSpillStorage {
    Memory(Vec<Batch>),
    Disk(NamedTempFile),
}

struct SharedSpillInner {
    storage: SharedSpillStorage,
    schema: RowSchema,
    rows: usize,
    max_record_bytes: usize,
}

/// Immutable repeatable row materialization bounded by the source buffer's
/// memory budget and backed by a temporary file after that budget is exceeded.
#[derive(Clone)]
pub struct SharedSpill {
    inner: Arc<SharedSpillInner>,
}

impl SharedSpill {
    pub fn schema(&self) -> &[String] {
        self.inner.schema.columns()
    }

    pub fn row_schema(&self) -> &RowSchema {
        &self.inner.schema
    }

    pub fn rows(&self) -> usize {
        self.inner.rows
    }

    /// Whether this materialization crossed its memory budget and uses disk.
    pub fn has_spilled(&self) -> bool {
        matches!(self.inner.storage, SharedSpillStorage::Disk(_))
    }

    pub fn reader(&self) -> ExecResult<SharedSpillReader> {
        let source = Arc::clone(&self.inner);
        Self::reader_from_source(source)
    }

    /// Consume this materialization into a one-shot reader.
    ///
    /// When the in-memory materialization has no other owners, batches move
    /// directly into the reader instead of being deep-cloned. Shared and disk
    /// materializations retain the independent-reader behavior of [`Self::reader`].
    pub fn into_reader(self) -> ExecResult<SharedSpillReader> {
        match Arc::try_unwrap(self.inner) {
            Ok(SharedSpillInner {
                storage: SharedSpillStorage::Memory(batches),
                schema,
                max_record_bytes,
                ..
            }) => Ok(SharedSpillReader {
                reader: SharedSpillReaderSource::OwnedMemory(batches.into_iter()),
                source: None,
                failed: false,
                max_record_bytes,
                expected_schema: Some(schema),
            }),
            Ok(inner) => Self::reader_from_source(Arc::new(inner)),
            Err(source) => Self::reader_from_source(source),
        }
    }

    fn reader_from_source(source: Arc<SharedSpillInner>) -> ExecResult<SharedSpillReader> {
        let reader = match &source.storage {
            SharedSpillStorage::Memory(_) => SharedSpillReaderSource::Memory { next_batch: 0 },
            SharedSpillStorage::Disk(file) => {
                SharedSpillReaderSource::Disk(open_spill_reader(file)?)
            }
        };
        let max_record_bytes = source.max_record_bytes;
        let expected_schema = Some(source.schema.clone());
        Ok(SharedSpillReader {
            reader,
            source: Some(source),
            failed: false,
            max_record_bytes,
            expected_schema,
        })
    }

    /// Open an independent physical-row reader without collecting the spill's
    /// batches or row count in memory.
    pub fn read_rows(&self) -> ExecResult<SpillRows<SharedSpillReader>> {
        self.reader().map(SpillRows::new)
    }
}

enum SharedSpillReaderSource {
    Memory { next_batch: usize },
    OwnedMemory(std::vec::IntoIter<Batch>),
    Disk(BufReader<File>),
}

fn validate_decoded_schema(batch: Batch, expected_schema: Option<&RowSchema>) -> ExecResult<Batch> {
    if expected_schema.is_none_or(|expected| expected == &batch.schema) {
        return Ok(batch);
    }
    let expected = expected_schema.expect("schema presence checked above");
    Err(spill_error(format!(
        "spill batch schema mismatch: expected {:?}, got {:?}",
        expected.columns(),
        batch.schema.columns()
    )))
}

/// Reader for a [`SharedSpill`]. Independent readers retain the shared source;
/// a consuming reader may instead own unique in-memory batches directly.
pub struct SharedSpillReader {
    reader: SharedSpillReaderSource,
    source: Option<Arc<SharedSpillInner>>,
    failed: bool,
    max_record_bytes: usize,
    expected_schema: Option<RowSchema>,
}

impl Iterator for SharedSpillReader {
    type Item = ExecResult<Batch>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match &mut self.reader {
            SharedSpillReaderSource::Memory { next_batch } => {
                let source = self
                    .source
                    .as_ref()
                    .expect("shared memory reader retains its source");
                let SharedSpillStorage::Memory(batches) = &source.storage else {
                    unreachable!("shared materialization reader/storage mismatch")
                };
                let batch = batches.get(*next_batch)?.clone();
                *next_batch += 1;
                Some(validate_decoded_schema(
                    batch,
                    self.expected_schema.as_ref(),
                ))
            }
            SharedSpillReaderSource::OwnedMemory(batches) => batches
                .next()
                .map(|batch| validate_decoded_schema(batch, self.expected_schema.as_ref())),
            SharedSpillReaderSource::Disk(reader) => {
                match read_bounded_spill_record(reader, self.max_record_bytes, "shared spill batch")
                {
                    Ok(None) => None,
                    Ok(Some(record)) => {
                        let decoded = decode_batch(&record).and_then(|batch| {
                            validate_decoded_schema(batch, self.expected_schema.as_ref())
                        });
                        if decoded.is_err() {
                            self.failed = true;
                        }
                        Some(decoded)
                    }
                    Err(error) => {
                        self.failed = true;
                        Some(Err(spill_error(format!(
                            "failed to read shared spill batch: {error}"
                        ))))
                    }
                }
            }
        }
    }
}

/// Restoring iterator returned by [`SpillBuffer::drain`].
pub struct SpillDrain {
    reader: Option<BufReader<File>>,
    // Keep the named file alive until disk iteration finishes or the iterator
    // is dropped. Its Drop implementation unlinks the temporary file.
    spill_file: Option<NamedTempFile>,
    memory: std::vec::IntoIter<Batch>,
    disk_finished: bool,
    failed: bool,
    max_record_bytes: usize,
    expected_schema: Option<RowSchema>,
}

impl Iterator for SpillDrain {
    type Item = ExecResult<Batch>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }

        if !self.disk_finished {
            let Some(reader) = self.reader.as_mut() else {
                self.failed = true;
                self.disk_finished = true;
                return Some(Err(spill_error(
                    "spill drain entered disk phase without a reader",
                )));
            };
            match read_bounded_spill_record(reader, self.max_record_bytes, "spill batch") {
                Ok(None) => {
                    self.disk_finished = true;
                    self.reader = None;
                    self.spill_file = None;
                }
                Ok(Some(record)) => {
                    let decoded = decode_batch(&record).and_then(|batch| {
                        validate_decoded_schema(batch, self.expected_schema.as_ref())
                    });
                    if decoded.is_err() {
                        self.failed = true;
                    }
                    return Some(decoded);
                }
                Err(error) => {
                    self.failed = true;
                    return Some(Err(spill_error(format!(
                        "failed to read spill batch: {error}"
                    ))));
                }
            }
        }

        self.memory
            .next()
            .map(|batch| validate_decoded_schema(batch, self.expected_schema.as_ref()))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let lower = if self.disk_finished {
            self.memory.len()
        } else {
            0
        };
        (lower, None)
    }
}

/// Repeatable, non-consuming batch reader returned by [`SpillBuffer::reader`].
pub struct SpillReader<'a> {
    reader: Option<BufReader<File>>,
    memory: std::slice::Iter<'a, Batch>,
    disk_finished: bool,
    failed: bool,
    max_record_bytes: usize,
    expected_schema: Option<RowSchema>,
}

impl Iterator for SpillReader<'_> {
    type Item = ExecResult<Batch>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }

        if !self.disk_finished {
            let Some(reader) = self.reader.as_mut() else {
                self.failed = true;
                self.disk_finished = true;
                return Some(Err(spill_error(
                    "spill reader entered disk phase without a file reader",
                )));
            };
            match read_bounded_spill_record(reader, self.max_record_bytes, "spill batch") {
                Ok(None) => {
                    self.disk_finished = true;
                    self.reader = None;
                }
                Ok(Some(record)) => {
                    let decoded = decode_batch(&record).and_then(|batch| {
                        validate_decoded_schema(batch, self.expected_schema.as_ref())
                    });
                    if decoded.is_err() {
                        self.failed = true;
                    }
                    return Some(decoded);
                }
                Err(error) => {
                    self.failed = true;
                    return Some(Err(spill_error(format!(
                        "failed to read spill batch: {error}"
                    ))));
                }
            }
        }

        self.memory
            .next()
            .cloned()
            .map(|batch| validate_decoded_schema(batch, self.expected_schema.as_ref()))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let lower = if self.disk_finished {
            self.memory.len()
        } else {
            0
        };
        (lower, None)
    }
}

/// Physical-row flattening adapter for [`SpillReader`] and [`SpillDrain`].
pub struct SpillRows<I> {
    batches: I,
    current_schema: Option<RowSchema>,
    current: std::vec::IntoIter<PhysicalRow>,
}

impl<I> SpillRows<I> {
    fn new(batches: I) -> Self {
        Self {
            batches,
            current_schema: None,
            current: Vec::new().into_iter(),
        }
    }
}

impl<I> Iterator for SpillRows<I>
where
    I: Iterator<Item = ExecResult<Batch>>,
{
    type Item = ExecResult<OwnedPhysicalRow>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(row) = self.current.next() {
                let schema = self
                    .current_schema
                    .as_ref()
                    .expect("spill row iterator retains the current batch schema")
                    .clone();
                return Some(Ok(OwnedPhysicalRow::new(schema, row)));
            }
            match self.batches.next()? {
                Ok(batch) => {
                    self.current_schema = Some(batch.schema);
                    self.current = batch.rows.into_iter();
                }
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

#[cfg(test)]
mod tests;
