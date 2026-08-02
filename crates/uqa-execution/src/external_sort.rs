//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Byte-bounded external merge sort.
//!
//! Input is divided into stable sorted runs whose exact spill encoding stays
//! within `work_mem_bytes` (except for one indivisible oversized row). This is
//! a hard bound on retained encoded row/run bytes, not Rust allocator resident
//! bytes. Runs are written through [`crate::spill::SpillBuffer`], then merged
//! with a fixed fan-in. Merge reader/heap overhead is therefore constant: at
//! most [`EXTERNAL_SORT_MERGE_FAN_IN`] decoded rows plus one byte-bounded output
//! buffer, independent of the total run count.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::PathBuf;

use uqa_core::Value;
use uqa_sql::ResultRow;

use crate::batch::{Batch, RowSchema, DEFAULT_BATCH_SIZE};
use crate::physical::{ExecError, ExecResult, PhysicalOperator, PhysicalOrder};
use crate::relational::{compare_sort_key_values, SharedExpressionEvaluator, SortKey};
use crate::spill::SpillBuffer;

/// Maximum number of input runs opened by one merge operation.
pub const EXTERNAL_SORT_MERGE_FAN_IN: usize = 16;

const RUN_KEYS: &str = "__uqa_external_sort_keys";
const RUN_SEQUENCE: &str = "__uqa_external_sort_sequence";
const RUN_ROW: &str = "__uqa_external_sort_row";

fn run_schema() -> RowSchema {
    RowSchema::new(vec![
        RUN_KEYS.to_string(),
        RUN_SEQUENCE.to_string(),
        RUN_ROW.to_string(),
    ])
}

/// Physical external sort with stable SQL ordering and optional global top-K.
pub struct ExternalSort<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    keys: Vec<SortKey>,
    evaluator: SharedExpressionEvaluator<'a>,
    keep: Option<usize>,
    work_mem_bytes: usize,
    spill_directory: Option<PathBuf>,
    schema: RowSchema,
    ordering: Vec<PhysicalOrder>,
    output: Option<Box<dyn Iterator<Item = ExecResult<ResultRow>> + Send>>,
    initial_run_count: usize,
    merge_pass_count: usize,
}

impl<'a> ExternalSort<'a> {
    pub fn new(
        child: Box<dyn PhysicalOperator + 'a>,
        keys: Vec<SortKey>,
        evaluator: SharedExpressionEvaluator<'a>,
        keep: Option<usize>,
        work_mem_bytes: usize,
    ) -> Self {
        let schema = RowSchema::new(child.schema().to_vec());
        let ordering = keys
            .iter()
            .map(|key| match &key.expr {
                crate::ScalarExpr::Column(column) => Some(PhysicalOrder {
                    column: column.clone(),
                    descending: key.descending,
                    nulls_first: Some(key.nulls_first.unwrap_or(key.descending)),
                    nullable: true,
                }),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
        Self {
            child,
            keys,
            evaluator,
            keep,
            work_mem_bytes,
            spill_directory: None,
            schema,
            ordering,
            output: None,
            initial_run_count: 0,
            merge_pass_count: 0,
        }
    }

    /// Place sort runs in a caller-selected temporary-data directory.
    pub fn with_spill_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.spill_directory = Some(directory.into());
        self
    }

    pub fn initial_run_count(&self) -> usize {
        self.initial_run_count
    }

    pub fn merge_pass_count(&self) -> usize {
        self.merge_pass_count
    }

    fn create_run_buffer(&self) -> SpillBuffer {
        self.spill_directory.as_ref().map_or_else(
            || SpillBuffer::new(self.work_mem_bytes),
            |directory| SpillBuffer::new_in(self.work_mem_bytes, directory),
        )
    }

    fn build_initial_runs(&mut self) -> ExecResult<Vec<SortedRun>> {
        let schema = run_schema();
        let mut sequence = 0_u64;
        let mut pending = Vec::new();
        let mut pending_bytes = 0_usize;
        let mut runs = Vec::new();

        while let Some(batch) = self.child.next()? {
            for row in batch.rows {
                let mut key_values = Vec::with_capacity(self.keys.len());
                for key in &self.keys {
                    key_values.push(self.evaluator.evaluate(&key.expr, &row)?);
                }
                let record = encode_record(DecoratedRow {
                    keys: key_values,
                    sequence,
                    row,
                });
                sequence = sequence.checked_add(1).ok_or_else(|| {
                    ExecError::Other("external sort input sequence overflow".into())
                })?;
                let (record, record_bytes) = encoded_record_size(&schema, record)?;
                let would_exceed = pending_bytes
                    .checked_add(record_bytes)
                    .is_none_or(|bytes| bytes > self.work_mem_bytes);

                if would_exceed && !pending.is_empty() {
                    if let Some(run) = self.finish_run(std::mem::take(&mut pending), true)? {
                        runs.push(run);
                    }
                    pending_bytes = 0;
                }

                pending.push(record);
                pending_bytes = pending_bytes.checked_add(record_bytes).ok_or_else(|| {
                    ExecError::Other("external sort pending-byte count overflow".into())
                })?;

                // One row is indivisible. Flush it immediately when it alone is
                // larger than work_mem so no additional row joins it in memory.
                if pending_bytes > self.work_mem_bytes {
                    if let Some(run) = self.finish_run(std::mem::take(&mut pending), true)? {
                        runs.push(run);
                    }
                    pending_bytes = 0;
                }
            }
        }

        let force_spill = !runs.is_empty();
        if let Some(run) = self.finish_run(pending, force_spill)? {
            runs.push(run);
        }
        Ok(runs)
    }

    fn finish_run(
        &self,
        records: Vec<ResultRow>,
        force_spill: bool,
    ) -> ExecResult<Option<SortedRun>> {
        let mut records = records
            .into_iter()
            .map(|record| decode_record(record, self.keys.len()))
            .collect::<ExecResult<Vec<_>>>()?;
        records.sort_unstable_by(|left, right| {
            compare_sort_key_values(&self.keys, &left.keys, &right.keys)
                .then_with(|| left.sequence.cmp(&right.sequence))
        });
        if let Some(keep) = self.keep {
            records.truncate(keep);
        }
        if records.is_empty() {
            return Ok(None);
        }

        let mut buffer = self.create_run_buffer();
        let schema = run_schema();
        let mut writer = RunBatchWriter::new(schema);
        for record in records {
            writer.push(&mut buffer, encode_record(record))?;
        }
        writer.finish(&mut buffer)?;
        if force_spill {
            buffer.spill_pending()?;
        }
        Ok(Some(SortedRun { buffer }))
    }

    fn collapse_runs(&mut self, mut runs: Vec<SortedRun>) -> ExecResult<Option<SortedRun>> {
        while runs.len() > 1 {
            self.merge_pass_count = self.merge_pass_count.checked_add(1).ok_or_else(|| {
                ExecError::Other("external sort merge-pass count overflow".into())
            })?;
            let mut inputs = runs.into_iter();
            let mut merged = Vec::new();
            loop {
                let group: Vec<_> = inputs.by_ref().take(EXTERNAL_SORT_MERGE_FAN_IN).collect();
                if group.is_empty() {
                    break;
                }
                merged.push(merge_group(
                    group,
                    &self.keys,
                    self.keep,
                    self.create_run_buffer(),
                )?);
            }
            runs = merged;
        }
        Ok(runs.pop())
    }
}

impl PhysicalOperator for ExternalSort<'_> {
    fn schema(&self) -> &[String] {
        &self.schema.columns
    }

    fn output_ordering(&self) -> &[PhysicalOrder] {
        &self.ordering
    }

    fn open(&mut self) -> ExecResult<()> {
        self.output = None;
        self.initial_run_count = 0;
        self.merge_pass_count = 0;
        self.child.open()?;

        let runs = self.build_initial_runs()?;
        self.initial_run_count = runs.len();
        let Some(mut final_run) = self.collapse_runs(runs)? else {
            self.output = Some(Box::new(std::iter::empty()));
            return Ok(());
        };
        let rows = final_run.buffer.drain_rows()?;
        let expected_key_count = self.keys.len();
        self.output = Some(Box::new(rows.map(move |record| {
            record
                .and_then(|record| decode_record(record, expected_key_count))
                .map(|record| record.row)
        })));
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(output) = self.output.as_mut() else {
            return Ok(None);
        };
        let mut rows = Vec::with_capacity(DEFAULT_BATCH_SIZE);
        for _ in 0..DEFAULT_BATCH_SIZE {
            match output.next() {
                Some(Ok(row)) => rows.push(row),
                Some(Err(error)) => return Err(error),
                None => break,
            }
        }
        if rows.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Batch::new(self.schema.clone(), rows)))
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.output = None;
        self.child.close()
    }
}

struct SortedRun {
    buffer: SpillBuffer,
}

struct DecoratedRow {
    keys: Vec<Value>,
    sequence: u64,
    row: ResultRow,
}

fn encode_record(record: DecoratedRow) -> ResultRow {
    BTreeMap::from([
        (RUN_KEYS.to_string(), Value::List(record.keys)),
        (
            RUN_SEQUENCE.to_string(),
            Value::Bytes(record.sequence.to_be_bytes().to_vec()),
        ),
        (RUN_ROW.to_string(), Value::Map(record.row)),
    ])
}

fn decode_record(mut record: ResultRow, expected_key_count: usize) -> ExecResult<DecoratedRow> {
    let keys = match record.remove(RUN_KEYS) {
        Some(Value::List(keys)) => keys,
        _ => return Err(ExecError::Other("invalid external sort run keys".into())),
    };
    if keys.len() != expected_key_count {
        return Err(ExecError::Other(format!(
            "invalid external sort run key count: expected {expected_key_count}, got {}",
            keys.len()
        )));
    }
    let sequence = match record.remove(RUN_SEQUENCE) {
        Some(Value::Bytes(bytes)) if bytes.len() == std::mem::size_of::<u64>() => {
            let bytes: [u8; 8] = bytes
                .try_into()
                .map_err(|_| ExecError::Other("invalid external sort run sequence width".into()))?;
            u64::from_be_bytes(bytes)
        }
        _ => {
            return Err(ExecError::Other(
                "invalid external sort run sequence".into(),
            ))
        }
    };
    let row = match record.remove(RUN_ROW) {
        Some(Value::Map(row)) => row,
        _ => return Err(ExecError::Other("invalid external sort run row".into())),
    };
    Ok(DecoratedRow {
        keys,
        sequence,
        row,
    })
}

fn encoded_record_size(schema: &RowSchema, record: ResultRow) -> ExecResult<(ResultRow, usize)> {
    let bytes = SpillBuffer::encoded_single_row_size(schema, &record)?;
    Ok((record, bytes))
}

struct RunBatchWriter {
    schema: RowSchema,
    pending: Vec<ResultRow>,
    conservative_bytes: usize,
}

impl RunBatchWriter {
    fn new(schema: RowSchema) -> Self {
        Self {
            schema,
            pending: Vec::new(),
            conservative_bytes: 0,
        }
    }

    fn push(&mut self, output: &mut SpillBuffer, record: ResultRow) -> ExecResult<()> {
        let (record, record_bytes) = encoded_record_size(&self.schema, record)?;
        let exceeds_budget = self
            .conservative_bytes
            .checked_add(record_bytes)
            .is_none_or(|bytes| bytes > output.budget_bytes());
        if exceeds_budget && !self.pending.is_empty() {
            self.flush(output)?;
        }
        self.pending.push(record);
        self.conservative_bytes = self
            .conservative_bytes
            .checked_add(record_bytes)
            .ok_or_else(|| ExecError::Other("external sort run batch size overflow".into()))?;
        if self.conservative_bytes > output.budget_bytes() {
            self.flush(output)?;
        }
        Ok(())
    }

    fn finish(mut self, output: &mut SpillBuffer) -> ExecResult<()> {
        self.flush(output)
    }

    fn flush(&mut self, output: &mut SpillBuffer) -> ExecResult<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        output.push(Batch::new(
            self.schema.clone(),
            std::mem::take(&mut self.pending),
        ))?;
        self.conservative_bytes = 0;
        Ok(())
    }
}

type RowStream = Box<dyn Iterator<Item = ExecResult<ResultRow>> + Send>;

struct MergeCursor {
    rows: RowStream,
}

struct HeapItem {
    record: DecoratedRow,
    cursor: usize,
}

fn merge_group(
    runs: Vec<SortedRun>,
    keys: &[SortKey],
    keep: Option<usize>,
    mut output: SpillBuffer,
) -> ExecResult<SortedRun> {
    debug_assert!(!runs.is_empty());
    debug_assert!(runs.len() <= EXTERNAL_SORT_MERGE_FAN_IN);
    let mut cursors = Vec::with_capacity(runs.len());
    let mut heap = Vec::with_capacity(runs.len());

    for run in runs {
        let mut buffer = run.buffer;
        let rows: RowStream = Box::new(buffer.drain_rows()?);
        cursors.push(MergeCursor { rows });
        let cursor = cursors.len() - 1;
        if let Some(record) = cursors[cursor].rows.next().transpose()? {
            heap_push(
                &mut heap,
                HeapItem {
                    record: decode_record(record, keys.len())?,
                    cursor,
                },
                keys,
            );
        }
    }

    let schema = run_schema();
    let mut writer = RunBatchWriter::new(schema);
    let mut emitted = 0_usize;
    while !heap.is_empty() && keep.is_none_or(|keep| emitted < keep) {
        let item = heap_pop(&mut heap, keys)
            .ok_or_else(|| ExecError::Other("external sort merge heap became empty".into()))?;
        let cursor = item.cursor;
        writer.push(&mut output, encode_record(item.record))?;
        emitted = emitted
            .checked_add(1)
            .ok_or_else(|| ExecError::Other("external sort emitted-row count overflow".into()))?;
        if let Some(record) = cursors[cursor].rows.next().transpose()? {
            heap_push(
                &mut heap,
                HeapItem {
                    record: decode_record(record, keys.len())?,
                    cursor,
                },
                keys,
            );
        }
    }
    writer.finish(&mut output)?;
    output.spill_pending()?;
    Ok(SortedRun { buffer: output })
}

fn compare_heap_items(keys: &[SortKey], left: &HeapItem, right: &HeapItem) -> Ordering {
    compare_sort_key_values(keys, &left.record.keys, &right.record.keys)
        .then_with(|| left.record.sequence.cmp(&right.record.sequence))
}

fn heap_push(heap: &mut Vec<HeapItem>, item: HeapItem, keys: &[SortKey]) {
    heap.push(item);
    let mut child = heap.len() - 1;
    while child > 0 {
        let parent = (child - 1) / 2;
        if compare_heap_items(keys, &heap[child], &heap[parent]) != Ordering::Less {
            break;
        }
        heap.swap(child, parent);
        child = parent;
    }
}

fn heap_pop(heap: &mut Vec<HeapItem>, keys: &[SortKey]) -> Option<HeapItem> {
    if heap.is_empty() {
        return None;
    }
    let smallest = heap.swap_remove(0);
    let mut parent = 0;
    loop {
        let left = parent * 2 + 1;
        if left >= heap.len() {
            break;
        }
        let right = left + 1;
        let child = if right < heap.len()
            && compare_heap_items(keys, &heap[right], &heap[left]) == Ordering::Less
        {
            right
        } else {
            left
        };
        if compare_heap_items(keys, &heap[child], &heap[parent]) != Ordering::Less {
            break;
        }
        heap.swap(parent, child);
        parent = child;
    }
    Some(smallest)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::Arc;

    use super::*;
    use crate::physical::run_to_rows;
    use crate::scalar::ScalarExpr;
    use crate::scan::TableScan;

    struct Columns;

    impl crate::relational::ExpressionEvaluator for Columns {
        fn evaluate(&self, expression: &ScalarExpr, row: &ResultRow) -> ExecResult<Value> {
            match expression {
                ScalarExpr::Column(name) => Ok(row.get(name).cloned().unwrap_or(Value::Null)),
                _ => Err(ExecError::Other(
                    "test evaluator only supports columns".into(),
                )),
            }
        }
    }

    fn row(key: i64, input: i64) -> ResultRow {
        BTreeMap::from([
            ("key".into(), Value::Int(key)),
            ("input".into(), Value::Int(input)),
        ])
    }

    fn sort(rows: Vec<ResultRow>, budget: usize, keep: Option<usize>) -> ExternalSort<'static> {
        ExternalSort::new(
            Box::new(TableScan::from_rows(
                vec!["key".into(), "input".into()],
                rows,
            )),
            vec![SortKey {
                expr: ScalarExpr::Column("key".into()),
                descending: false,
                nulls_first: None,
            }],
            Arc::new(Columns),
            keep,
            budget,
        )
    }

    fn int_column(rows: &[ResultRow], column: &str) -> Vec<i64> {
        rows.iter()
            .map(|row| match row.get(column) {
                Some(Value::Int(value)) => *value,
                value => panic!("unexpected {column} value: {value:?}"),
            })
            .collect()
    }

    #[test]
    fn tiny_budget_builds_many_runs_and_multi_pass_merge() {
        let rows = (0..(EXTERNAL_SORT_MERGE_FAN_IN as i64 * 2 + 5))
            .rev()
            .map(|value| row(value, value))
            .collect();
        let mut operator = sort(rows, 1, None);
        operator.open().unwrap();
        assert!(operator.initial_run_count() > EXTERNAL_SORT_MERGE_FAN_IN);
        assert!(operator.merge_pass_count() >= 2);
        let mut output = Vec::new();
        while let Some(batch) = operator.next().unwrap() {
            output.extend(batch.rows);
        }
        operator.close().unwrap();
        assert_eq!(
            int_column(&output, "key"),
            (0..(EXTERNAL_SORT_MERGE_FAN_IN as i64 * 2 + 5)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn equal_keys_keep_original_input_order_across_runs() {
        let rows = (0..40).map(|input| row(7, input)).collect();
        let mut operator = sort(rows, 1, None);
        let (_, output) = run_to_rows(&mut operator).unwrap();
        assert_eq!(int_column(&output, "input"), (0..40).collect::<Vec<_>>());
    }

    #[test]
    fn keep_is_global_top_k_across_multiple_runs() {
        let rows = (0..100).rev().map(|value| row(value, value)).collect();
        let mut operator = sort(rows, 1, Some(7));
        let (_, output) = run_to_rows(&mut operator).unwrap();
        assert_eq!(int_column(&output, "key"), (0..7).collect::<Vec<_>>());
    }

    #[test]
    fn spill_creation_error_is_propagated() {
        let not_a_directory = tempfile::NamedTempFile::new().unwrap();
        let mut operator = sort(vec![row(1, 0)], 0, None)
            .with_spill_directory(not_a_directory.path().to_path_buf());
        let error = operator.open().unwrap_err();
        assert!(error.to_string().contains("failed to create spill file"));
    }

    #[test]
    fn large_budget_single_run_does_not_create_a_spill_file() {
        let not_a_directory = tempfile::NamedTempFile::new().unwrap();
        let rows = (0..20).rev().map(|value| row(value, value)).collect();
        let mut operator =
            sort(rows, 1_000_000, None).with_spill_directory(not_a_directory.path().to_path_buf());
        let (_, output) = run_to_rows(&mut operator).unwrap();
        assert_eq!(int_column(&output, "key"), (0..20).collect::<Vec<_>>());
        assert_eq!(operator.initial_run_count(), 1);
        assert_eq!(operator.merge_pass_count(), 0);
    }

    #[test]
    fn corrupt_run_read_error_is_propagated() {
        let keys = vec![SortKey {
            expr: ScalarExpr::Column("key".into()),
            descending: false,
            nulls_first: None,
        }];
        let record = encode_record(DecoratedRow {
            keys: vec![Value::Int(1)],
            sequence: 0,
            row: row(1, 0),
        });
        let mut buffer = SpillBuffer::new(0);
        buffer.push(Batch::new(run_schema(), vec![record])).unwrap();
        let path = buffer.spill_path().unwrap().to_path_buf();
        let mut corrupt = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        corrupt.write_all(&1_u64.to_le_bytes()).unwrap();
        corrupt.write_all(&[0xff]).unwrap();
        corrupt.flush().unwrap();

        let result = merge_group(vec![SortedRun { buffer }], &keys, None, SpillBuffer::new(0));
        let error = match result {
            Ok(_) => panic!("corrupt run unexpectedly merged"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("truncated schema column count"));
    }

    #[test]
    fn corrupt_run_key_width_is_reported_before_comparison() {
        let record = encode_record(DecoratedRow {
            keys: Vec::new(),
            sequence: 0,
            row: row(1, 0),
        });
        let error = match decode_record(record, 1) {
            Ok(_) => panic!("corrupt run record unexpectedly decoded"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("invalid external sort run key count"));
    }

    #[test]
    fn empty_input_and_zero_keep_are_empty() {
        let mut empty = sort(Vec::new(), 1, None);
        assert!(run_to_rows(&mut empty).unwrap().1.is_empty());

        let rows = (0..10).map(|value| row(value, value)).collect();
        let mut zero = sort(rows, 1, Some(0));
        assert!(run_to_rows(&mut zero).unwrap().1.is_empty());
    }
}
