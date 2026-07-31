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

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Serialize, Serializer};
use tempfile::NamedTempFile;
use uqa_core::{DecimalValue, TemporalValue, Value};
use uqa_sql::ResultRow;

use crate::batch::{Batch, RowSchema};
use crate::physical::{ExecError, ExecResult};

const SPILL_MAGIC: &[u8] = b"UQA-SPILL\x01\n";

/// Append-only batch buffer with an encoded-byte memory budget.
///
/// The budget is exact for the serialized representation and does not claim to
/// be the Rust allocator's resident-byte accounting. At most one incoming or
/// decoded batch can itself be larger than the budget; successful pushes do not
/// retain such an oversized batch in memory.
pub struct SpillBuffer {
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
    /// delimiter written to disk.
    pub fn encoded_size(batch: &Batch) -> ExecResult<usize> {
        let mut counter = ByteCounter::default();
        serde_json::to_writer(&mut counter, &ExactBatch(batch))
            .map_err(|error| spill_error(format!("failed to size spill batch: {error}")))?;
        counter
            .bytes
            .checked_add(1)
            .ok_or_else(|| spill_error("spill batch encoded size overflow"))
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
        })
    }

    /// Open a repeatable row stream without materializing all buffered rows.
    pub fn read_rows(&self) -> ExecResult<SpillRows<SpillReader<'_>>> {
        self.reader().map(SpillRows::new)
    }

    /// Drain buffered batches in their original input order.
    ///
    /// The returned iterator owns the temporary file. Each disk read or decode
    /// failure is returned as an [`ExecError`], and dropping the iterator early
    /// still removes the temporary file.
    pub fn drain(&mut self) -> ExecResult<SpillDrain> {
        let reader = self
            .spill_file
            .as_ref()
            .map(open_spill_reader)
            .transpose()?;
        let spill_file = self.spill_file.take();
        let memory = std::mem::take(&mut self.batches).into_iter();

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
        })
    }

    /// Drain and materialize every restored batch.
    pub fn drain_all(&mut self) -> ExecResult<Vec<Batch>> {
        self.drain()?.collect()
    }

    /// Consume the buffer as a row stream without materializing all batches.
    pub fn drain_rows(&mut self) -> ExecResult<SpillRows<SpillDrain>> {
        self.drain().map(SpillRows::new)
    }

    /// Discard all buffered data and remove any spill file.
    pub fn clear(&mut self) {
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
    pub fn into_shared(mut self, schema: Vec<String>) -> ExecResult<SharedSpill> {
        let rows = self.rows;
        let storage = if self.spill_file.is_none() {
            SharedSpillStorage::Memory(std::mem::take(&mut self.batches))
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
    schema: Vec<String>,
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
        let reader = match &source.storage {
            SharedSpillStorage::Memory(_) => SharedSpillReaderSource::Memory { next_batch: 0 },
            SharedSpillStorage::Disk(file) => {
                SharedSpillReaderSource::Disk(open_spill_reader(file)?)
            }
        };
        Ok(SharedSpillReader {
            reader,
            source,
            failed: false,
            max_record_bytes: self.inner.max_record_bytes,
        })
    }

    /// Open an independent row-at-a-time reader without materializing the
    /// spill's batches or row count in memory.
    pub fn read_rows(&self) -> ExecResult<SpillRows<SharedSpillReader>> {
        self.reader().map(SpillRows::new)
    }
}

enum SharedSpillReaderSource {
    Memory { next_batch: usize },
    Disk(BufReader<File>),
}

/// Independent reader for a [`SharedSpill`]. The `Arc` keeps either the
/// in-memory batches or named file alive until the last scan has completed.
pub struct SharedSpillReader {
    reader: SharedSpillReaderSource,
    source: Arc<SharedSpillInner>,
    failed: bool,
    max_record_bytes: usize,
}

impl Iterator for SharedSpillReader {
    type Item = ExecResult<Batch>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match &mut self.reader {
            SharedSpillReaderSource::Memory { next_batch } => {
                let SharedSpillStorage::Memory(batches) = &self.source.storage else {
                    unreachable!("shared materialization reader/storage mismatch")
                };
                let batch = batches.get(*next_batch)?.clone();
                *next_batch += 1;
                Some(Ok(batch))
            }
            SharedSpillReaderSource::Disk(reader) => {
                match read_bounded_spill_record(reader, self.max_record_bytes, "shared spill batch")
                {
                    Ok(None) => None,
                    Ok(Some(record)) => {
                        let decoded = decode_batch(&record);
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
                    let decoded = decode_batch(&record);
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

        self.memory.next().map(Ok)
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
                    let decoded = decode_batch(&record);
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

        self.memory.next().cloned().map(Ok)
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

/// Row-flattening adapter for [`SpillReader`] and [`SpillDrain`].
pub struct SpillRows<I> {
    batches: I,
    current: std::vec::IntoIter<ResultRow>,
}

/// Disk-only row store with constant-memory positional lookup.
///
/// Each row is encoded with the same exact tagged representation as
/// [`SpillBuffer`]. Record offsets live in a second temporary file, so even a
/// partition with billions of rows does not create an in-memory offset table.
/// A single decoded row is the only input-sized allocation retained by
/// [`Self::get`]. Both files are unlinked by `NamedTempFile` on drop.
pub struct IndexedSpill {
    data: NamedTempFile,
    offsets: NamedTempFile,
    rows: u64,
    encoded_bytes: u64,
}

impl IndexedSpill {
    pub fn new() -> ExecResult<Self> {
        Ok(Self {
            data: NamedTempFile::new().map_err(|error| {
                spill_error(format!("failed to create indexed spill data: {error}"))
            })?,
            offsets: NamedTempFile::new().map_err(|error| {
                spill_error(format!("failed to create indexed spill offsets: {error}"))
            })?,
            rows: 0,
            encoded_bytes: 0,
        })
    }

    pub fn len(&self) -> u64 {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn encoded_bytes(&self) -> u64 {
        self.encoded_bytes
    }

    /// Append one indivisible row. Failed writes roll both files back to their
    /// original lengths, so callers never observe a partial index entry.
    pub fn push(&mut self, row: &ResultRow) -> ExecResult<()> {
        let payload = serde_json::to_vec(&ExactRow(row))
            .map_err(|error| spill_error(format!("failed to encode indexed spill row: {error}")))?;
        let length = u64::try_from(payload.len())
            .map_err(|_| spill_error("indexed spill row is too large"))?;
        // Validate every piece of metadata before touching either file.  A
        // counter overflow after the append would otherwise return an error
        // while leaving a physically visible row whose offset/count was not
        // published consistently.
        let next_rows = self
            .rows
            .checked_add(1)
            .ok_or_else(|| spill_error("indexed spill row count overflow"))?;
        let record_bytes = length
            .checked_add(8)
            .ok_or_else(|| spill_error("indexed spill row length overflow"))?;
        let next_encoded_bytes = self
            .encoded_bytes
            .checked_add(record_bytes)
            .ok_or_else(|| spill_error("indexed spill byte count overflow"))?;
        let data_length = self
            .data
            .as_file_mut()
            .seek(SeekFrom::End(0))
            .map_err(|error| spill_error(format!("failed to seek indexed spill data: {error}")))?;
        let offsets_length =
            self.offsets
                .as_file_mut()
                .seek(SeekFrom::End(0))
                .map_err(|error| {
                    spill_error(format!("failed to seek indexed spill offsets: {error}"))
                })?;

        let write_result = (|| -> std::io::Result<()> {
            self.data.as_file_mut().write_all(&length.to_le_bytes())?;
            self.data.as_file_mut().write_all(&payload)?;
            self.data.as_file_mut().flush()?;
            self.offsets
                .as_file_mut()
                .write_all(&data_length.to_le_bytes())?;
            self.offsets.as_file_mut().flush()
        })();
        if let Err(error) = write_result {
            let data_rollback = self.data.as_file_mut().set_len(data_length);
            let offsets_rollback = self.offsets.as_file_mut().set_len(offsets_length);
            let rollback_error = match (data_rollback, offsets_rollback) {
                (Ok(()), Ok(())) => None,
                (Err(data), Ok(())) => Some(format!("data rollback failed: {data}")),
                (Ok(()), Err(offsets)) => Some(format!("offset rollback failed: {offsets}")),
                (Err(data), Err(offsets)) => Some(format!(
                    "data rollback failed: {data}; offset rollback failed: {offsets}"
                )),
            };
            if let Some(rollback) = rollback_error {
                return Err(spill_error(format!(
                    "failed to append indexed spill row: {error}; {rollback}"
                )));
            }
            return Err(spill_error(format!(
                "failed to append indexed spill row: {error}"
            )));
        }

        self.rows = next_rows;
        self.encoded_bytes = next_encoded_bytes;
        Ok(())
    }

    /// Decode the row at `index` without loading any other row or index entry.
    pub fn get(&mut self, index: u64) -> ExecResult<ResultRow> {
        if index >= self.rows {
            return Err(spill_error(format!(
                "indexed spill row {index} is outside 0..{}",
                self.rows
            )));
        }
        let expected_offsets_length = self
            .rows
            .checked_mul(8)
            .ok_or_else(|| spill_error("indexed spill offsets length overflow"))?;
        let actual_offsets_length = self
            .offsets
            .as_file()
            .metadata()
            .map_err(|error| {
                spill_error(format!("failed to inspect indexed spill offsets: {error}"))
            })?
            .len();
        if actual_offsets_length != expected_offsets_length {
            return Err(spill_error(format!(
                "indexed spill offsets length {actual_offsets_length} does not match expected {expected_offsets_length}"
            )));
        }
        let data_length = self
            .data
            .as_file()
            .metadata()
            .map_err(|error| spill_error(format!("failed to inspect indexed spill data: {error}")))?
            .len();
        let offset_position = index
            .checked_mul(8)
            .ok_or_else(|| spill_error("indexed spill offset overflow"))?;
        let offset = read_indexed_offset(self.offsets.as_file_mut(), offset_position)?;
        let record_end = if index
            .checked_add(1)
            .ok_or_else(|| spill_error("indexed spill row index overflow"))?
            < self.rows
        {
            read_indexed_offset(
                self.offsets.as_file_mut(),
                offset_position
                    .checked_add(8)
                    .ok_or_else(|| spill_error("indexed spill next offset overflow"))?,
            )?
        } else {
            data_length
        };
        let payload_start = offset
            .checked_add(8)
            .ok_or_else(|| spill_error("indexed spill payload offset overflow"))?;
        if payload_start > record_end || record_end > data_length {
            return Err(spill_error(format!(
                "indexed spill record bounds {offset}..{record_end} are outside data length {data_length}"
            )));
        }
        self.data
            .as_file_mut()
            .seek(SeekFrom::Start(offset))
            .map_err(|error| spill_error(format!("failed to seek indexed spill row: {error}")))?;
        let mut length = [0_u8; 8];
        self.data
            .as_file_mut()
            .read_exact(&mut length)
            .map_err(|error| {
                spill_error(format!("failed to read indexed spill length: {error}"))
            })?;
        let declared_length = u64::from_le_bytes(length);
        let available_length = record_end - payload_start;
        if declared_length != available_length {
            return Err(spill_error(format!(
                "indexed spill row length {declared_length} does not match record payload {available_length}"
            )));
        }
        let length = usize::try_from(declared_length)
            .map_err(|_| spill_error("indexed spill row length is outside address space"))?;
        let mut payload = Vec::new();
        payload.try_reserve_exact(length).map_err(|error| {
            spill_error(format!(
                "unable to allocate indexed spill row payload of {length} bytes: {error}"
            ))
        })?;
        payload.resize(length, 0);
        self.data
            .as_file_mut()
            .read_exact(&mut payload)
            .map_err(|error| spill_error(format!("failed to read indexed spill row: {error}")))?;
        decode_row(&payload)
    }
}

fn read_indexed_offset(file: &mut File, position: u64) -> ExecResult<u64> {
    file.seek(SeekFrom::Start(position))
        .map_err(|error| spill_error(format!("failed to seek indexed spill offset: {error}")))?;
    let mut encoded = [0_u8; 8];
    file.read_exact(&mut encoded)
        .map_err(|error| spill_error(format!("failed to read indexed spill offset: {error}")))?;
    Ok(u64::from_le_bytes(encoded))
}

impl<I> SpillRows<I> {
    fn new(batches: I) -> Self {
        Self {
            batches,
            current: Vec::new().into_iter(),
        }
    }
}

impl<I> Iterator for SpillRows<I>
where
    I: Iterator<Item = ExecResult<Batch>>,
{
    type Item = ExecResult<ResultRow>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(row) = self.current.next() {
                return Some(Ok(row));
            }
            match self.batches.next()? {
                Ok(batch) => self.current = batch.rows.into_iter(),
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

#[derive(Default)]
struct ByteCounter {
    bytes: usize,
}

impl Write for ByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.checked_add(buffer.len()).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::FileTooLarge, "encoded size overflow")
        })?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn spill_error(message: impl Into<String>) -> ExecError {
    ExecError::Other(message.into())
}

fn read_bounded_spill_record<R: BufRead>(
    reader: &mut R,
    max_record_bytes: usize,
    description: &str,
) -> ExecResult<Option<Vec<u8>>> {
    let mut record = Vec::new();
    loop {
        let (chunk_len, terminated) = {
            let available = reader
                .fill_buf()
                .map_err(|error| spill_error(format!("failed to read {description}: {error}")))?;
            if available.is_empty() {
                if record.is_empty() {
                    return Ok(None);
                }
                return Err(spill_error(format!(
                    "truncated {description}: missing record delimiter"
                )));
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(index) => (index + 1, true),
                None => (available.len(), false),
            }
        };

        let next_len = record
            .len()
            .checked_add(chunk_len)
            .ok_or_else(|| spill_error(format!("{description} length overflow")))?;
        if next_len > max_record_bytes {
            return Err(spill_error(format!(
                "{description} exceeds recorded maximum of {max_record_bytes} bytes"
            )));
        }
        record.try_reserve(chunk_len).map_err(|error| {
            spill_error(format!(
                "unable to allocate {chunk_len} more bytes for {description}: {error}"
            ))
        })?;
        let available = reader
            .fill_buf()
            .map_err(|error| spill_error(format!("failed to read {description}: {error}")))?;
        record.extend_from_slice(&available[..chunk_len]);
        reader.consume(chunk_len);

        if terminated {
            let delimiter = record.pop();
            debug_assert_eq!(delimiter, Some(b'\n'));
            return Ok(Some(record));
        }
    }
}

fn open_spill_reader(file: &NamedTempFile) -> ExecResult<BufReader<File>> {
    let reopened = file
        .reopen()
        .map_err(|error| spill_error(format!("failed to reopen spill file: {error}")))?;
    let mut reader = BufReader::new(reopened);
    let mut magic = [0_u8; SPILL_MAGIC.len()];
    reader
        .read_exact(&mut magic)
        .map_err(|error| spill_error(format!("failed to read spill header: {error}")))?;
    if magic != SPILL_MAGIC {
        return Err(spill_error("invalid spill file header"));
    }
    Ok(reader)
}

fn append_batches(file: &mut File, batches: &[Batch]) -> ExecResult<()> {
    let original_len = file
        .seek(SeekFrom::End(0))
        .map_err(|error| spill_error(format!("failed to seek spill file: {error}")))?;

    let result = {
        let mut writer = BufWriter::new(&mut *file);
        let result = (|| {
            for batch in batches {
                serde_json::to_writer(&mut writer, &ExactBatch(batch)).map_err(|error| {
                    spill_error(format!("failed to serialize spill batch: {error}"))
                })?;
                writer.write_all(b"\n").map_err(|error| {
                    spill_error(format!("failed to write spill batch: {error}"))
                })?;
            }
            writer
                .flush()
                .map_err(|error| spill_error(format!("failed to flush spill file: {error}")))
        })();
        drop(writer);
        result
    };

    if let Err(error) = result {
        if let Err(rollback_error) = file.set_len(original_len) {
            return Err(spill_error(format!(
                "{error}; failed to roll back partial spill write: {rollback_error}"
            )));
        }
        file.seek(SeekFrom::End(0)).map_err(|rollback_error| {
            spill_error(format!(
                "{error}; failed to restore spill position: {rollback_error}"
            ))
        })?;
        return Err(error);
    }

    Ok(())
}

struct ExactBatch<'a>(&'a Batch);

impl Serialize for ExactBatch<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("schema", &self.0.schema.columns)?;
        map.serialize_entry("rows", &ExactRows(&self.0.rows))?;
        map.end()
    }
}

struct ExactRows<'a>(&'a [ResultRow]);

impl Serialize for ExactRows<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            sequence.serialize_element(&ExactRow(row))?;
        }
        sequence.end()
    }
}

struct ExactRow<'a>(&'a ResultRow);

impl Serialize for ExactRow<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in self.0 {
            map.serialize_entry(name, &ExactValue(value))?;
        }
        map.end()
    }
}

struct ExactValue<'a>(&'a Value);

impl Serialize for ExactValue<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.0 {
            Value::Null => serializer.serialize_unit_variant("SpillValue", 0, "Null"),
            Value::Bool(value) => {
                serializer.serialize_newtype_variant("SpillValue", 1, "Bool", value)
            }
            Value::Int(value) => {
                serializer.serialize_newtype_variant("SpillValue", 2, "Int", value)
            }
            Value::Float(value) => {
                serializer.serialize_newtype_variant("SpillValue", 3, "Float", &value.to_bits())
            }
            Value::Str(value) => {
                serializer.serialize_newtype_variant("SpillValue", 4, "Str", value)
            }
            Value::Bytes(value) => {
                serializer.serialize_newtype_variant("SpillValue", 5, "Bytes", value)
            }
            Value::Temporal(value) => {
                serializer.serialize_newtype_variant("SpillValue", 6, "Temporal", value)
            }
            Value::Decimal(value) => serializer.serialize_newtype_variant(
                "SpillValue",
                7,
                "Decimal",
                &value.to_sql_string(),
            ),
            Value::List(value) => {
                serializer.serialize_newtype_variant("SpillValue", 8, "List", &ExactValues(value))
            }
            Value::Map(value) => {
                serializer.serialize_newtype_variant("SpillValue", 9, "Map", &ExactMap(value))
            }
        }
    }
}

struct ExactValues<'a>(&'a [Value]);

impl Serialize for ExactValues<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for value in self.0 {
            sequence.serialize_element(&ExactValue(value))?;
        }
        sequence.end()
    }
}

struct ExactMap<'a>(&'a BTreeMap<String, Value>);

impl Serialize for ExactMap<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, &ExactValue(value))?;
        }
        map.end()
    }
}

#[derive(Deserialize)]
struct StoredBatch {
    schema: Vec<String>,
    rows: Vec<BTreeMap<String, StoredValue>>,
}

fn decode_row(record: &[u8]) -> ExecResult<ResultRow> {
    let stored: BTreeMap<String, StoredValue> =
        serde_json::from_slice(record).map_err(|error| {
            spill_error(format!("failed to deserialize indexed spill row: {error}"))
        })?;
    stored
        .into_iter()
        .map(|(name, value)| value.into_value().map(|value| (name, value)))
        .collect()
}

#[derive(Deserialize)]
enum StoredValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(String),
    Bytes(Vec<u8>),
    Temporal(TemporalValue),
    Decimal(String),
    List(Vec<StoredValue>),
    Map(BTreeMap<String, StoredValue>),
}

impl StoredValue {
    fn into_value(self) -> ExecResult<Value> {
        match self {
            Self::Null => Ok(Value::Null),
            Self::Bool(value) => Ok(Value::Bool(value)),
            Self::Int(value) => Ok(Value::Int(value)),
            Self::Float(bits) => Ok(Value::Float(f64::from_bits(bits))),
            Self::Str(value) => Ok(Value::Str(value)),
            Self::Bytes(value) => Ok(Value::Bytes(value)),
            Self::Temporal(value) => Ok(Value::Temporal(value)),
            Self::Decimal(value) => DecimalValue::parse(&value)
                .map(Value::Decimal)
                .ok_or_else(|| spill_error(format!("invalid decimal in spill file: {value}"))),
            Self::List(values) => values
                .into_iter()
                .map(Self::into_value)
                .collect::<ExecResult<Vec<_>>>()
                .map(Value::List),
            Self::Map(values) => values
                .into_iter()
                .map(|(key, value)| value.into_value().map(|value| (key, value)))
                .collect::<ExecResult<BTreeMap<_, _>>>()
                .map(Value::Map),
        }
    }
}

fn decode_batch(record: &[u8]) -> ExecResult<Batch> {
    let stored: StoredBatch = serde_json::from_slice(record)
        .map_err(|error| spill_error(format!("failed to deserialize spill batch: {error}")))?;
    let rows = stored
        .rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|(name, value)| value.into_value().map(|value| (name, value)))
                .collect::<ExecResult<ResultRow>>()
        })
        .collect::<ExecResult<Vec<_>>>()?;
    Ok(Batch::new(RowSchema::new(stored.schema), rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_batch(start: usize, n: usize) -> Batch {
        let schema = RowSchema::new(vec!["x".into()]);
        let rows = (start..start + n)
            .map(|value| BTreeMap::from([("x".into(), Value::Int(value as i64))]))
            .collect();
        Batch::new(schema, rows)
    }

    #[test]
    fn low_budget_creates_file_and_round_trips_in_order() {
        let mut buffer = SpillBuffer::new(1);
        assert!(buffer.push(dummy_batch(0, 2)).unwrap());

        let path = buffer.spill_path().unwrap().to_path_buf();
        assert!(path.is_file());
        assert!(std::fs::metadata(&path).unwrap().len() > SPILL_MAGIC.len() as u64);
        assert_eq!(buffer.rows(), 2);
        assert_eq!(buffer.in_memory_rows(), 0);
        assert_eq!(buffer.in_memory_bytes(), 0);
        assert_eq!(buffer.spilled_rows(), 2);
        assert_eq!(buffer.spilled_batches(), 1);
        assert!(buffer.spilled_bytes() > 1);

        buffer.push(dummy_batch(2, 1)).unwrap();
        let restored = buffer.drain_all().unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].schema.columns, vec!["x"]);
        assert_eq!(restored[0].rows, dummy_batch(0, 2).rows);
        assert_eq!(restored[1].rows, dummy_batch(2, 1).rows);
        assert!(!path.exists());
        assert_eq!(buffer.rows(), 0);
    }

    #[test]
    fn multiple_spills_preserve_batch_order() {
        let mut buffer = SpillBuffer::new(1);
        buffer.push(dummy_batch(0, 3)).unwrap();
        buffer.push(dummy_batch(3, 2)).unwrap();
        buffer.push(dummy_batch(5, 2)).unwrap();
        buffer.push(dummy_batch(7, 1)).unwrap();

        let restored = buffer.drain_all().unwrap();
        let values: Vec<i64> = restored
            .into_iter()
            .flat_map(|batch| batch.rows)
            .map(|row| match row.get("x") {
                Some(Value::Int(value)) => *value,
                value => panic!("unexpected restored value: {value:?}"),
            })
            .collect();
        assert_eq!(values, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn indexed_spill_reads_large_partitions_without_an_offset_vector() {
        let mut spill = IndexedSpill::new().unwrap();
        for value in 0..4096_i64 {
            spill
                .push(&BTreeMap::from([
                    ("id".into(), Value::Int(value)),
                    (
                        "payload".into(),
                        Value::List(vec![Value::Str(format!("row-{value}"))]),
                    ),
                ]))
                .unwrap();
        }
        assert_eq!(spill.len(), 4096);
        assert!(spill.encoded_bytes() > 4096 * 8);
        for index in [4095_u64, 0, 2048, 17] {
            let row = spill.get(index).unwrap();
            assert_eq!(row.get("id"), Some(&Value::Int(index as i64)));
        }
        assert!(spill.get(4096).unwrap_err().to_string().contains("outside"));
    }

    #[test]
    fn indexed_spill_rejects_corrupt_length_before_payload_allocation() {
        let mut spill = IndexedSpill::new().unwrap();
        spill
            .push(&BTreeMap::from([("id".into(), Value::Int(1))]))
            .unwrap();
        spill.data.as_file_mut().seek(SeekFrom::Start(0)).unwrap();
        spill
            .data
            .as_file_mut()
            .write_all(&u64::MAX.to_le_bytes())
            .unwrap();
        spill.data.as_file_mut().flush().unwrap();

        let error = spill.get(0).unwrap_err();
        assert!(error.to_string().contains("does not match record payload"));
    }

    #[test]
    fn indexed_spill_rejects_corrupt_record_offsets() {
        let mut spill = IndexedSpill::new().unwrap();
        spill
            .push(&BTreeMap::from([("id".into(), Value::Int(1))]))
            .unwrap();
        spill
            .push(&BTreeMap::from([("id".into(), Value::Int(2))]))
            .unwrap();
        spill
            .offsets
            .as_file_mut()
            .seek(SeekFrom::Start(8))
            .unwrap();
        spill
            .offsets
            .as_file_mut()
            .write_all(&0_u64.to_le_bytes())
            .unwrap();
        spill.offsets.as_file_mut().flush().unwrap();

        let error = spill.get(0).unwrap_err();
        assert!(error.to_string().contains("record bounds"));
    }

    #[test]
    fn indexed_spill_rejects_metadata_overflow_before_writing() {
        let row = BTreeMap::from([("id".into(), Value::Int(1))]);

        let mut row_overflow = IndexedSpill::new().unwrap();
        row_overflow.rows = u64::MAX;
        let error = row_overflow.push(&row).unwrap_err();
        assert!(error.to_string().contains("row count overflow"));
        assert_eq!(row_overflow.data.as_file().metadata().unwrap().len(), 0);
        assert_eq!(row_overflow.offsets.as_file().metadata().unwrap().len(), 0);

        let mut byte_overflow = IndexedSpill::new().unwrap();
        byte_overflow.encoded_bytes = u64::MAX;
        let error = byte_overflow.push(&row).unwrap_err();
        assert!(error.to_string().contains("byte count overflow"));
        assert_eq!(byte_overflow.data.as_file().metadata().unwrap().len(), 0);
        assert_eq!(byte_overflow.offsets.as_file().metadata().unwrap().len(), 0);
    }

    #[test]
    fn spill_buffer_rejects_counter_overflow_before_writing() {
        let mut total_overflow = SpillBuffer::new(0);
        total_overflow.rows = usize::MAX;
        let error = total_overflow.push(dummy_batch(0, 1)).unwrap_err();
        assert!(error.to_string().contains("row count overflow"));
        assert!(total_overflow.spill_path().is_none());
        assert!(total_overflow.batches.is_empty());

        let mut spill_stats_overflow = SpillBuffer::unbounded();
        spill_stats_overflow.push(dummy_batch(0, 1)).unwrap();
        spill_stats_overflow.spilled_batches = usize::MAX;
        let error = spill_stats_overflow.spill_pending().unwrap_err();
        assert!(error.to_string().contains("batch count overflow"));
        assert!(spill_stats_overflow.spill_path().is_none());
        assert_eq!(spill_stats_overflow.batches.len(), 1);
    }

    #[test]
    fn exact_value_variants_and_float_bits_round_trip() {
        let decimal = DecimalValue::parse("-12.7500").unwrap();
        let values = BTreeMap::from([
            ("bytes".into(), Value::Bytes(vec![1, 2, 3])),
            (
                "list".into(),
                Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
            ),
            (
                "nan".into(),
                Value::Float(f64::from_bits(0x7ff8_0000_0000_0042)),
            ),
            ("negative_zero".into(), Value::Float(-0.0)),
            ("decimal".into(), Value::Decimal(decimal)),
            (
                "temporal".into(),
                Value::Temporal(TemporalValue::Interval {
                    months: 14,
                    days: 3,
                    micros: 4_000_000,
                }),
            ),
            (
                "tagged_map".into(),
                Value::Map(BTreeMap::from([
                    ("$uqa_type".into(), Value::Str("date".into())),
                    ("days".into(), Value::Int(3)),
                ])),
            ),
        ]);
        let batch = Batch::new(
            RowSchema::new(values.keys().cloned().collect()),
            vec![values],
        );
        let expected = batch.rows.clone();

        let mut buffer = SpillBuffer::new(0);
        buffer.push(batch).unwrap();
        let restored = buffer.drain_all().unwrap();
        let actual = &restored[0].rows;

        for key in ["bytes", "list", "decimal", "temporal", "tagged_map"] {
            assert_eq!(actual[0].get(key), expected[0].get(key));
        }
        for key in ["nan", "negative_zero"] {
            let Some(Value::Float(actual)) = actual[0].get(key) else {
                panic!("missing float {key}");
            };
            let Some(Value::Float(expected)) = expected[0].get(key) else {
                unreachable!();
            };
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
    }

    #[test]
    fn creation_failure_is_returned_without_losing_memory_rows() {
        let not_a_directory = NamedTempFile::new().unwrap();
        let mut buffer = SpillBuffer::new_in(0, not_a_directory.path());
        let error = buffer.push(dummy_batch(0, 1)).unwrap_err();
        assert!(error.to_string().contains("failed to create spill file"));
        assert_eq!(buffer.rows(), 1);
        assert_eq!(buffer.in_memory_rows(), 1);
        assert!(buffer.in_memory_bytes() > 0);
        assert!(!buffer.has_spilled());
        assert_eq!(buffer.drain_all().unwrap()[0].rows, dummy_batch(0, 1).rows);
    }

    #[test]
    fn corrupted_spill_record_surfaces_decode_error_and_cleans_up() {
        let mut buffer = SpillBuffer::new(0);
        buffer.push(dummy_batch(0, 1)).unwrap();
        let path = buffer.spill_path().unwrap().to_path_buf();
        buffer
            .spill_file
            .as_mut()
            .unwrap()
            .as_file_mut()
            .write_all(b"not-json\n")
            .unwrap();
        buffer
            .spill_file
            .as_mut()
            .unwrap()
            .as_file_mut()
            .flush()
            .unwrap();

        let mut drain = buffer.drain().unwrap();
        assert_eq!(drain.next().unwrap().unwrap().rows, dummy_batch(0, 1).rows);
        let error = drain.next().unwrap().unwrap_err();
        assert!(error
            .to_string()
            .contains("failed to deserialize spill batch"));
        assert!(drain.next().is_none());
        drop(drain);
        assert!(!path.exists());
    }

    #[test]
    fn corrupted_spill_record_is_bounded_by_written_record_metadata() {
        let mut buffer = SpillBuffer::new(0);
        buffer.push(dummy_batch(0, 1)).unwrap();
        let max_record_bytes = buffer.max_spilled_record_bytes;
        assert!(max_record_bytes > 0);
        let file = buffer.spill_file.as_mut().unwrap().as_file_mut();
        file.write_all(&vec![b'x'; max_record_bytes]).unwrap();
        file.write_all(b"\n").unwrap();
        file.flush().unwrap();

        let mut reader = buffer.reader().unwrap();
        assert!(reader.next().unwrap().is_ok());
        let error = reader.next().unwrap().unwrap_err();
        assert!(error.to_string().contains("exceeds recorded maximum"));
        assert!(reader.next().is_none());
    }

    #[test]
    fn truncated_spill_record_is_not_accepted_at_eof() {
        let mut buffer = SpillBuffer::new(0);
        buffer.push(dummy_batch(0, 1)).unwrap();
        let file = buffer.spill_file.as_mut().unwrap().as_file_mut();
        file.write_all(b"truncated").unwrap();
        file.flush().unwrap();

        let mut reader = buffer.reader().unwrap();
        assert!(reader.next().unwrap().is_ok());
        let error = reader.next().unwrap().unwrap_err();
        assert!(error.to_string().contains("missing record delimiter"));
        assert!(reader.next().is_none());
    }

    #[test]
    fn dropping_buffer_or_drain_removes_temporary_file() {
        let path = {
            let mut buffer = SpillBuffer::new(0);
            buffer.push(dummy_batch(0, 1)).unwrap();
            buffer.spill_path().unwrap().to_path_buf()
        };
        assert!(!path.exists());

        let (path, drain) = {
            let mut buffer = SpillBuffer::new(0);
            buffer.push(dummy_batch(0, 1)).unwrap();
            let path = buffer.spill_path().unwrap().to_path_buf();
            let drain = buffer.drain().unwrap();
            (path, drain)
        };
        assert!(path.exists());
        drop(drain);
        assert!(!path.exists());
    }

    #[test]
    fn encoded_byte_budget_never_retains_an_oversized_batch() {
        let small = dummy_batch(0, 1);
        let budget = SpillBuffer::encoded_size(&small).unwrap();
        let mut buffer = SpillBuffer::new(budget);

        assert!(!buffer.push(small).unwrap());
        assert_eq!(buffer.in_memory_bytes(), budget);
        assert!(buffer.in_memory_bytes() <= buffer.budget_bytes());

        assert!(buffer.push(dummy_batch(1, 1)).unwrap());
        assert!(buffer.in_memory_bytes() <= buffer.budget_bytes());

        let oversized = Batch::new(
            RowSchema::new(vec!["payload".into()]),
            vec![BTreeMap::from([(
                "payload".into(),
                Value::Bytes(vec![7; budget * 2]),
            )])],
        );
        assert!(SpillBuffer::encoded_size(&oversized).unwrap() > budget);
        assert!(buffer.push(oversized).unwrap());
        assert_eq!(buffer.in_memory_bytes(), 0);
        assert!(buffer.has_spilled());
    }

    #[test]
    fn reader_is_repeatable_and_row_stream_does_not_collect_all_batches() {
        let budget = SpillBuffer::encoded_size(&dummy_batch(0, 1)).unwrap();
        let mut buffer = SpillBuffer::new(budget);
        buffer.push(dummy_batch(0, 1)).unwrap();
        buffer.push(dummy_batch(1, 1)).unwrap();
        buffer.push(dummy_batch(2, 1)).unwrap();

        let read = || {
            buffer
                .read_rows()
                .unwrap()
                .map(|row| match row.unwrap().get("x") {
                    Some(Value::Int(value)) => *value,
                    value => panic!("unexpected streamed value: {value:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(read(), vec![0, 1, 2]);
        assert_eq!(read(), vec![0, 1, 2]);
        assert_eq!(buffer.rows(), 3);

        let drained = buffer
            .drain_rows()
            .unwrap()
            .map(|row| row.unwrap())
            .collect::<Vec<_>>();
        assert_eq!(drained.len(), 3);
        assert_eq!(buffer.rows(), 0);
    }

    #[test]
    fn shared_materialization_keeps_in_budget_batches_in_memory() {
        let mut buffer = SpillBuffer::unbounded();
        buffer.push(dummy_batch(0, 2)).unwrap();
        buffer.push(dummy_batch(2, 2)).unwrap();
        let shared = buffer.into_shared(vec!["x".into()]).unwrap();

        assert!(!shared.has_spilled());
        let read = || {
            shared
                .read_rows()
                .unwrap()
                .map(|row| match row.unwrap().get("x") {
                    Some(Value::Int(value)) => *value,
                    value => panic!("unexpected shared value: {value:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(read(), vec![0, 1, 2, 3]);
        assert_eq!(read(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn shared_materialization_flushes_in_memory_tail_after_spilling() {
        let budget = SpillBuffer::encoded_size(&dummy_batch(0, 1)).unwrap();
        let mut buffer = SpillBuffer::new(budget);
        buffer.push(dummy_batch(0, 1)).unwrap();
        assert!(buffer.push(dummy_batch(1, 1)).unwrap());
        assert!(buffer.has_spilled());
        assert_eq!(buffer.in_memory_rows(), 1);

        let shared = buffer.into_shared(vec!["x".into()]).unwrap();
        assert!(shared.has_spilled());
        let read = || {
            shared
                .read_rows()
                .unwrap()
                .map(|row| match row.unwrap().get("x") {
                    Some(Value::Int(value)) => *value,
                    value => panic!("unexpected shared value: {value:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(read(), vec![0, 1]);
        assert_eq!(read(), vec![0, 1]);
    }
}
