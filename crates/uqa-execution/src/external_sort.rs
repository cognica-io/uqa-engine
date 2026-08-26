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
use std::path::PathBuf;

use uqa_core::Value;

use crate::batch::{Batch, PhysicalRow, RowSchema};
use crate::physical::{
    order_expression_position, ExecError, ExecResult, PhysicalOperator, PhysicalOrder,
};
use crate::relational::{compare_sort_key_values_by, SharedExpressionEvaluator, SortKey};
use crate::spill::{EncodedBatchSizer, SpillBuffer, SpillDrain};

/// Maximum number of input runs opened by one merge operation.
pub const EXTERNAL_SORT_MERGE_FAN_IN: usize = 16;

fn run_schema(source_width: usize, key_count: usize) -> RowSchema {
    RowSchema::with_internal_relation_types(
        uqa_sql::ast::InternalRelationId::allocate(),
        vec![None; source_width + key_count + 1],
    )
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
    input_slots: Vec<usize>,
    run_schema: RowSchema,
    ordering: Vec<PhysicalOrder>,
    output: Option<SpillDrain>,
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
        let (schema, input_slots) = child.row_schema().canonical_projection();
        let run_schema = run_schema(input_slots.len(), keys.len());
        let ordering = keys
            .iter()
            .map(|key| {
                order_expression_position(&schema, &key.expr).map(|position| PhysicalOrder {
                    position,
                    descending: key.descending,
                    nulls_first: Some(key.nulls_first.unwrap_or(key.descending)),
                    nullable: true,
                })
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
            input_slots,
            run_schema,
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
        let mut sequence = 0_u64;
        let mut pending = Vec::new();
        let mut pending_size = EncodedBatchSizer::new(&self.run_schema)?;
        let mut runs = Vec::new();

        while let Some(batch) = self.child.next()? {
            for row in batch.rows {
                let mut key_values = Vec::with_capacity(self.keys.len());
                for key in &self.keys {
                    key_values.push(self.evaluator.evaluate_physical(
                        &key.expr,
                        &batch.schema,
                        &row,
                    )?);
                }
                let record_sequence = sequence;
                key_values.push(Value::Bytes(record_sequence.to_be_bytes().to_vec()));
                let record = DecoratedRow {
                    row: row
                        .project_slots(&self.input_slots)
                        .append_values(key_values),
                    sequence,
                };
                sequence = sequence.checked_add(1).ok_or_else(|| {
                    ExecError::Other("external sort input sequence overflow".into())
                })?;
                let mut candidate_size = pending_size;
                candidate_size.append(&record.row)?;
                let would_exceed = candidate_size.bytes() > self.work_mem_bytes;

                if would_exceed && !pending.is_empty() {
                    if let Some(run) = self.finish_run(std::mem::take(&mut pending), true)? {
                        runs.push(run);
                    }
                    pending_size = EncodedBatchSizer::new(&self.run_schema)?;
                    candidate_size = pending_size;
                    candidate_size.append(&record.row)?;
                }

                pending.push(record);
                pending_size = candidate_size;

                // One row is indivisible. Flush it immediately when it alone is
                // larger than work_mem so no additional row joins it in memory.
                if pending_size.bytes() > self.work_mem_bytes {
                    if let Some(run) = self.finish_run(std::mem::take(&mut pending), true)? {
                        runs.push(run);
                    }
                    pending_size = EncodedBatchSizer::new(&self.run_schema)?;
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
        records: Vec<DecoratedRow>,
        force_spill: bool,
    ) -> ExecResult<Option<SortedRun>> {
        let mut records = records;
        records.sort_unstable_by(|left, right| {
            compare_records(
                &self.keys,
                &self.run_schema,
                self.input_slots.len(),
                left,
                right,
            )
        });
        if let Some(keep) = self.keep {
            records.truncate(keep);
        }
        if records.is_empty() {
            return Ok(None);
        }

        let mut buffer = self.create_run_buffer();
        let mut writer = RunBatchWriter::new(self.run_schema.clone())?;
        for record in records {
            writer.push(&mut buffer, record.row)?;
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
                    &self.run_schema,
                    self.input_slots.len(),
                )?);
            }
            runs = merged;
        }
        Ok(runs.pop())
    }
}

impl PhysicalOperator for ExternalSort<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
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
            return Ok(());
        };
        self.output = Some(final_run.buffer.drain()?);
        Ok(())
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(output) = self.output.as_mut() else {
            return Ok(None);
        };
        loop {
            let Some(batch) = output.next().transpose()? else {
                return Ok(None);
            };
            validate_run_batch(&batch, &self.run_schema)?;
            if batch.rows.is_empty() {
                continue;
            }
            let rows = batch
                .rows
                .into_iter()
                .map(|row| row.into_prefix(self.input_slots.len()))
                .collect();
            return Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)));
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
    row: PhysicalRow,
    sequence: u64,
}

fn decode_record(
    schema: &RowSchema,
    row: PhysicalRow,
    source_width: usize,
    expected_key_count: usize,
) -> ExecResult<DecoratedRow> {
    let sequence_position = source_width
        .checked_add(expected_key_count)
        .ok_or_else(|| ExecError::Other("external sort run width overflow".into()))?;
    if schema.physical_width() != sequence_position.saturating_add(1) {
        return Err(ExecError::Other(format!(
            "invalid external sort run key count: expected {expected_key_count}"
        )));
    }
    for index in source_width..sequence_position {
        if row.value(index).is_none() {
            return Err(ExecError::Other(format!(
                "external sort run is missing key {}",
                index - source_width
            )));
        }
    }
    let sequence = match row.value(sequence_position) {
        Some(Value::Bytes(bytes)) if bytes.len() == std::mem::size_of::<u64>() => {
            let bytes: [u8; 8] = bytes
                .as_slice()
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
    Ok(DecoratedRow { row, sequence })
}

fn validate_run_batch(batch: &Batch, expected: &RowSchema) -> ExecResult<()> {
    if &batch.schema == expected {
        Ok(())
    } else {
        Err(ExecError::Other(format!(
            "invalid external sort run schema: expected {:?}, got {:?}",
            expected.columns(),
            batch.schema.columns()
        )))
    }
}

fn compare_records(
    keys: &[SortKey],
    _schema: &RowSchema,
    source_width: usize,
    left: &DecoratedRow,
    right: &DecoratedRow,
) -> Ordering {
    compare_sort_key_values_by(keys, |index| {
        (
            left.row
                .value(source_width + index)
                .expect("validated external sort run key"),
            right
                .row
                .value(source_width + index)
                .expect("validated external sort run key"),
        )
    })
    .then_with(|| left.sequence.cmp(&right.sequence))
}

struct RunBatchWriter {
    schema: RowSchema,
    pending: Vec<PhysicalRow>,
    pending_size: EncodedBatchSizer,
}

impl RunBatchWriter {
    fn new(schema: RowSchema) -> ExecResult<Self> {
        let pending_size = EncodedBatchSizer::new(&schema)?;
        Ok(Self {
            schema,
            pending: Vec::new(),
            pending_size,
        })
    }

    fn push(&mut self, output: &mut SpillBuffer, record: PhysicalRow) -> ExecResult<()> {
        let mut candidate_size = self.pending_size;
        candidate_size.append(&record)?;
        let exceeds_budget = candidate_size.bytes() > output.budget_bytes();
        if exceeds_budget && !self.pending.is_empty() {
            self.flush(output)?;
            candidate_size = self.pending_size;
            candidate_size.append(&record)?;
        }
        self.pending.push(record);
        self.pending_size = candidate_size;
        if self.pending_size.bytes() > output.budget_bytes() {
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
        output.push(Batch::from_physical_rows(
            self.schema.clone(),
            std::mem::take(&mut self.pending),
        ))?;
        self.pending_size = EncodedBatchSizer::new(&self.schema)?;
        Ok(())
    }
}

struct MergeCursor {
    batches: SpillDrain,
    rows: std::vec::IntoIter<PhysicalRow>,
    schema: RowSchema,
    source_width: usize,
    key_count: usize,
}

impl MergeCursor {
    fn new(
        mut buffer: SpillBuffer,
        schema: RowSchema,
        source_width: usize,
        key_count: usize,
    ) -> ExecResult<Self> {
        Ok(Self {
            batches: buffer.drain()?,
            rows: Vec::new().into_iter(),
            schema,
            source_width,
            key_count,
        })
    }

    fn next_record(&mut self) -> ExecResult<Option<DecoratedRow>> {
        loop {
            if let Some(row) = self.rows.next() {
                return decode_record(&self.schema, row, self.source_width, self.key_count)
                    .map(Some);
            }
            let Some(batch) = self.batches.next().transpose()? else {
                return Ok(None);
            };
            validate_run_batch(&batch, &self.schema)?;
            self.rows = batch.rows.into_iter();
        }
    }
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
    run_schema: &RowSchema,
    source_width: usize,
) -> ExecResult<SortedRun> {
    debug_assert!(!runs.is_empty());
    debug_assert!(runs.len() <= EXTERNAL_SORT_MERGE_FAN_IN);
    let mut cursors = Vec::with_capacity(runs.len());
    let mut heap = Vec::with_capacity(runs.len());

    for run in runs {
        cursors.push(MergeCursor::new(
            run.buffer,
            run_schema.clone(),
            source_width,
            keys.len(),
        )?);
        let cursor = cursors.len() - 1;
        if let Some(record) = cursors[cursor].next_record()? {
            heap_push(
                &mut heap,
                HeapItem { record, cursor },
                keys,
                run_schema,
                source_width,
            );
        }
    }

    let mut writer = RunBatchWriter::new(run_schema.clone())?;
    let mut emitted = 0_usize;
    while !heap.is_empty() && keep.is_none_or(|keep| emitted < keep) {
        let item = heap_pop(&mut heap, keys, run_schema, source_width)
            .ok_or_else(|| ExecError::Other("external sort merge heap became empty".into()))?;
        let cursor = item.cursor;
        writer.push(&mut output, item.record.row)?;
        emitted = emitted
            .checked_add(1)
            .ok_or_else(|| ExecError::Other("external sort emitted-row count overflow".into()))?;
        if let Some(record) = cursors[cursor].next_record()? {
            heap_push(
                &mut heap,
                HeapItem { record, cursor },
                keys,
                run_schema,
                source_width,
            );
        }
    }
    writer.finish(&mut output)?;
    output.spill_pending()?;
    Ok(SortedRun { buffer: output })
}

fn compare_heap_items(
    keys: &[SortKey],
    schema: &RowSchema,
    source_width: usize,
    left: &HeapItem,
    right: &HeapItem,
) -> Ordering {
    compare_records(keys, schema, source_width, &left.record, &right.record)
}

fn heap_push(
    heap: &mut Vec<HeapItem>,
    item: HeapItem,
    keys: &[SortKey],
    schema: &RowSchema,
    source_width: usize,
) {
    heap.push(item);
    let mut child = heap.len() - 1;
    while child > 0 {
        let parent = (child - 1) / 2;
        if compare_heap_items(keys, schema, source_width, &heap[child], &heap[parent])
            != Ordering::Less
        {
            break;
        }
        heap.swap(child, parent);
        child = parent;
    }
}

fn heap_pop(
    heap: &mut Vec<HeapItem>,
    keys: &[SortKey],
    schema: &RowSchema,
    source_width: usize,
) -> Option<HeapItem> {
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
            && compare_heap_items(keys, schema, source_width, &heap[right], &heap[left])
                == Ordering::Less
        {
            right
        } else {
            left
        };
        if compare_heap_items(keys, schema, source_width, &heap[child], &heap[parent])
            != Ordering::Less
        {
            break;
        }
        heap.swap(parent, child);
        parent = child;
    }
    Some(smallest)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write as _;
    use std::sync::Arc;

    use super::*;
    use crate::physical::{run_to_batches, run_to_rows};
    use crate::scalar::ScalarExpr;
    use crate::scan::TableScan;
    use uqa_sql::expr::RowLookup as _;
    use uqa_sql::ResultRow;

    struct Columns;

    struct PhysicalRowsScan {
        schema: RowSchema,
        rows: Option<Vec<PhysicalRow>>,
    }

    impl PhysicalOperator for PhysicalRowsScan {
        fn row_schema(&self) -> &RowSchema {
            &self.schema
        }

        fn open(&mut self) -> ExecResult<()> {
            Ok(())
        }

        fn next(&mut self) -> ExecResult<Option<Batch>> {
            Ok(self
                .rows
                .take()
                .map(|rows| Batch::from_physical_rows(self.schema.clone(), rows)))
        }

        fn close(&mut self) -> ExecResult<()> {
            Ok(())
        }
    }

    impl crate::relational::ExpressionEvaluator for Columns {
        fn evaluate(
            &self,
            expression: &ScalarExpr,
            row: &dyn uqa_sql::expr::RowLookup,
        ) -> ExecResult<Value> {
            match expression {
                ScalarExpr::Column(name) => Ok(row.column(name).cloned().unwrap_or(Value::Null)),
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
            output.extend(batch.into_result_rows());
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
    fn spill_batch_schema_overhead_is_amortized_across_sort_rows() {
        let rows = (0..20_000).rev().map(|value| row(value, value)).collect();
        let mut operator = sort(rows, 4 * 1024 * 1024, None);

        operator.open().unwrap();
        assert_eq!(operator.initial_run_count(), 1);
        assert_eq!(operator.merge_pass_count(), 0);
        operator.close().unwrap();
    }

    #[test]
    fn mixed_lock_origins_add_metadata_for_origin_free_rows_to_the_batch_budget() {
        let schema = RowSchema::new(vec!["value".into()]);
        let plain = PhysicalRow::from_values(vec![Value::Int(1)]);
        let locked = PhysicalRow::from_values(vec![Value::Int(2)])
            .with_lock_origin(crate::RowLockOrigin::new("source", "public.source", 2));
        let overhead = EncodedBatchSizer::new(&schema).unwrap().bytes();
        let mut plain_size = EncodedBatchSizer::new(&schema).unwrap();
        plain_size.append(&plain).unwrap();
        let plain_record_bytes = plain_size.bytes() - overhead;
        let mut locked_size = EncodedBatchSizer::new(&schema).unwrap();
        locked_size.append(&locked).unwrap();
        let locked_record_bytes = locked_size.bytes() - overhead;
        let mut size = EncodedBatchSizer::new(&schema).unwrap();
        size.append(&plain).unwrap();
        let without_retroactive_metadata = overhead + plain_record_bytes + locked_record_bytes;
        size.append(&locked).unwrap();
        let batch = Batch::from_physical_rows(schema, vec![plain, locked]);

        assert_eq!(size.bytes(), SpillBuffer::encoded_size(&batch).unwrap());
        assert_eq!(size.bytes(), without_retroactive_metadata + 8);
    }

    #[test]
    fn corrupt_run_read_error_is_propagated() {
        let keys = vec![SortKey {
            expr: ScalarExpr::Column("key".into()),
            descending: false,
            nulls_first: None,
        }];
        let schema = run_schema(2, 1);
        let record = PhysicalRow::from_values(vec![
            Value::Int(1),
            Value::Int(0),
            Value::Int(1),
            Value::Bytes(0_u64.to_be_bytes().to_vec()),
        ]);
        let mut buffer = SpillBuffer::new(0);
        buffer
            .push(Batch::from_physical_rows(schema.clone(), vec![record]))
            .unwrap();
        let path = buffer.spill_path().unwrap().to_path_buf();
        let mut corrupt = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        corrupt.write_all(&1_u64.to_le_bytes()).unwrap();
        corrupt.write_all(&[0xff]).unwrap();
        corrupt.flush().unwrap();

        let result = merge_group(
            vec![SortedRun { buffer }],
            &keys,
            None,
            SpillBuffer::new(0),
            &schema,
            2,
        );
        let error = match result {
            Ok(_) => panic!("corrupt run unexpectedly merged"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("truncated schema physical width"));
    }

    #[test]
    fn corrupt_run_key_width_is_reported_before_comparison() {
        let schema = run_schema(2, 0);
        let record = PhysicalRow::from_values(vec![
            Value::Int(1),
            Value::Int(0),
            Value::Bytes(0_u64.to_be_bytes().to_vec()),
        ]);
        let error = match decode_record(&schema, record, 2, 1) {
            Ok(_) => panic!("corrupt run record unexpectedly decoded"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("invalid external sort run key count"));
    }

    #[test]
    fn forced_spill_preserves_hidden_alias_and_public_column_slots() {
        let base = RowSchema::new(vec!["value".into(), "key".into()]);
        let aliased = RowSchema::with_identity_aliases(
            &base,
            &[(crate::ColumnIdentity::qualified("source", "value"), 0)],
        );
        let schema = RowSchema::append(&aliased, &["value".into()]);
        let rows = vec![
            PhysicalRow::from_values(vec![Value::Str("source-b".into()), Value::Int(2)])
                .append_values(vec![Value::Str("projected-b".into())]),
            PhysicalRow::from_values(vec![Value::Str("source-a".into()), Value::Int(1)])
                .append_values(vec![Value::Str("projected-a".into())]),
        ];
        let scan = PhysicalRowsScan {
            schema,
            rows: Some(rows),
        };
        let mut operator = ExternalSort::new(
            Box::new(scan),
            vec![SortKey {
                expr: ScalarExpr::Column("key".into()),
                descending: false,
                nulls_first: None,
            }],
            Arc::new(Columns),
            None,
            1,
        );

        let batches = run_to_batches(&mut operator).unwrap();
        assert!(operator.initial_run_count() > 1);
        let values = batches
            .iter()
            .flat_map(|batch| {
                batch.rows.iter().map(|row| {
                    let view = batch.schema.view(row);
                    (
                        view.get("value").cloned(),
                        view.qualified_column("source", "value").cloned(),
                    )
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                (
                    Some(Value::Str("projected-a".into())),
                    Some(Value::Str("source-a".into()))
                ),
                (
                    Some(Value::Str("projected-b".into())),
                    Some(Value::Str("source-b".into()))
                ),
            ]
        );
    }

    #[test]
    fn forced_spill_preserves_duplicate_logical_columns_positionally() {
        let schema = RowSchema::new(vec!["value".into(), "value".into(), "key".into()]);
        let scan = PhysicalRowsScan {
            schema,
            rows: Some(vec![
                PhysicalRow::from_values(vec![
                    Value::Str("left-b".into()),
                    Value::Str("right-b".into()),
                    Value::Int(2),
                ]),
                PhysicalRow::from_values(vec![
                    Value::Str("left-a".into()),
                    Value::Str("right-a".into()),
                    Value::Int(1),
                ]),
            ]),
        };
        let mut operator = ExternalSort::new(
            Box::new(scan),
            vec![SortKey {
                expr: ScalarExpr::Column("key".into()),
                descending: false,
                nulls_first: None,
            }],
            Arc::new(Columns),
            None,
            1,
        );

        let batches = run_to_batches(&mut operator).unwrap();
        assert!(operator.initial_run_count() > 1);
        assert_eq!(operator.schema(), ["value", "value", "key"]);
        let values = batches
            .iter()
            .flat_map(|batch| {
                batch.rows.iter().map(|row| {
                    let view = batch.schema.view(row);
                    (view.value_at(0).cloned(), view.value_at(1).cloned())
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                (
                    Some(Value::Str("left-a".into())),
                    Some(Value::Str("right-a".into()))
                ),
                (
                    Some(Value::Str("left-b".into())),
                    Some(Value::Str("right-b".into()))
                ),
            ]
        );
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
