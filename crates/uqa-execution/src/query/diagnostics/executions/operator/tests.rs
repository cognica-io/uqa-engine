//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    query::output::{MaterializedResultSink, ResultRows},
    PhysicalRow,
};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::{CancellationToken, Value};
use uqa_storage::read_control::StorageReadControl;

#[derive(Default)]
struct Calls {
    closed: usize,
    borrowed: usize,
    demands: Vec<usize>,
    backward: usize,
}

struct Source {
    schema: RowSchema,
    calls: Arc<Mutex<Calls>>,
    position: usize,
    count: usize,
    closed: bool,
    fail_close: bool,
}

fn source(count: usize) -> (Box<Source>, Arc<Mutex<Calls>>) {
    let calls = Arc::new(Mutex::new(Calls::default()));
    (
        Box::new(Source {
            schema: RowSchema::new(vec!["value".into()]),
            calls: calls.clone(),
            position: 0,
            count,
            closed: false,
            fail_close: false,
        }),
        calls,
    )
}

impl Source {
    fn pull(&mut self, count: usize) -> Option<Batch> {
        let count = count.min(self.count - self.position);
        self.position += count;
        (count != 0).then(|| {
            Batch::from_physical_rows(
                self.schema.clone(),
                vec![PhysicalRow::from_values(vec![Value::Int(7)]); count],
            )
        })
    }
}

impl PhysicalOperator for Source {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn estimated_cardinality(&self) -> Option<u64> {
        Some(self.count as u64)
    }
    fn supports_batched_consumption(&self) -> bool {
        true
    }
    fn backward_scan_support(&self) -> BackwardScanSupport {
        BackwardScanSupport::Native
    }
    fn open(&mut self) -> ExecResult<()> {
        Ok(())
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        Ok(self.pull(1))
    }
    fn next_with_demand(&mut self, rows: NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.calls.lock().demands.push(rows.get());
        Ok(self.pull(rows.get()))
    }
    fn next_direction(&mut self, direction: PhysicalScanDirection) -> ExecResult<Option<Batch>> {
        if direction == PhysicalScanDirection::Backward {
            self.calls.lock().backward += 1;
            if self.position == 0 {
                return Ok(None);
            }
            self.position -= 1;
            return Ok(Some(Batch::from_physical_rows(
                self.schema.clone(),
                vec![PhysicalRow::from_values(vec![Value::Int(7)])],
            )));
        }
        self.next()
    }
    fn rewind(&mut self) -> ExecResult<()> {
        self.position = 0;
        Ok(())
    }
    fn close(&mut self) -> ExecResult<()> {
        if !self.closed {
            self.closed = true;
            self.calls.lock().closed += 1;
            if self.fail_close {
                return Err(SQLError::Routine {
                    sqlstate: "58030".into(),
                    message: "scan close failed".into(),
                }
                .into());
            }
        }
        Ok(())
    }
    fn consume_into_aggregate(
        &mut self,
        executor: &mut dyn crate::relational::AggregateExecutor,
    ) -> ExecResult<bool> {
        assert!(executor.supports_projected_rows());
        assert!(executor.supports_storage_borrowed_rows());
        while self.position < self.count {
            self.calls.lock().borrowed += 1;
            self.position += 1;
            let fields = [&Value::Int(7)];
            let slots = [crate::ProjectedValueSlot::Field(0)];
            executor.consume_projected_row(&crate::ProjectedRow::new(
                &self.schema,
                &slots,
                &fields,
                &[],
            ))?;
        }
        Ok(true)
    }
    fn collect_materialized(&mut self, sink: &mut MaterializedResultSink<'_>) -> ExecResult<bool> {
        while self.position < self.count {
            self.calls.lock().borrowed += 1;
            self.position += 1;
            sink.append(&[&Value::Int(7)])?;
        }
        Ok(true)
    }
}

#[test]
fn physical_demand_direction_rewind_and_close_keep_their_native_protocol() {
    let diagnostics = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65536);
    let scope = diagnostics.enter_measured(control.clone(), true).unwrap();
    let (input, calls) = source(8);
    let mut input = diagnostics.observe(input, "Test Scan").unwrap();
    assert!(input.supports_batched_consumption());
    assert_eq!(input.estimated_cardinality(), Some(8));
    assert_eq!(input.backward_scan_support(), BackwardScanSupport::Native);
    input.open().unwrap();
    assert_eq!(
        input
            .next_with_demand(NonZeroUsize::new(2).unwrap())
            .unwrap()
            .unwrap()
            .rows
            .len(),
        2
    );
    input
        .next_direction(PhysicalScanDirection::Backward)
        .unwrap()
        .unwrap();
    input.rewind().unwrap();
    input.next().unwrap().unwrap();
    input.close().unwrap();
    input.close().unwrap();
    let captured = scope.finish_execution().unwrap();
    assert_eq!(
        (
            captured.nodes[0].metrics.rows,
            captured.nodes[0].metrics.loops
        ),
        (4, 2)
    );
    assert!(captured.nodes[0].metrics.startup <= captured.nodes[0].metrics.total);
    assert_eq!(calls.lock().demands, [2]);
    assert_eq!(calls.lock().backward, 1);
    assert_eq!(calls.lock().closed, 1);
    drop((input, captured));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn constructed_unpulled_and_empty_inputs_have_distinct_loop_counts() {
    let diagnostics = QueryDiagnostics::default();
    let scope = diagnostics
        .enter_measured(StorageReadControl::with_limit(65536), false)
        .unwrap();
    let mut unused = diagnostics.observe(source(1).0, "Unused").unwrap();
    let mut opened = diagnostics.observe(source(1).0, "Opened").unwrap();
    let mut empty = diagnostics.observe(source(0).0, "Empty").unwrap();
    unused.close().unwrap();
    opened.open().unwrap();
    opened.close().unwrap();
    empty.open().unwrap();
    assert!(empty.next().unwrap().is_none());
    empty.close().unwrap();
    let captured = scope.finish_execution().unwrap();
    assert_eq!(
        captured
            .nodes
            .iter()
            .map(|n| n.metrics.loops)
            .collect::<Vec<_>>(),
        [0, 0, 1]
    );
    assert!(captured.nodes.iter().all(|n| n.metrics.total.is_zero()));
}

#[test]
fn nested_invocations_snapshot_open_cycles_without_freezing_the_parent() {
    let diagnostics = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65536);
    let outer = diagnostics.enter_measured(control.clone(), true).unwrap();
    let mut input = diagnostics.observe(source(8).0, "Retained Scan").unwrap();
    input.open().unwrap();
    input.next().unwrap().unwrap();
    let inner = diagnostics.enter_measured(control.clone(), false).unwrap();
    input.next().unwrap().unwrap();
    let child = inner.finish_execution().unwrap();
    input.next().unwrap().unwrap();
    let parent = outer.finish_execution().unwrap();
    assert_eq!(
        (child.nodes[0].metrics.rows, child.nodes[0].metrics.loops),
        (1, 1)
    );
    assert!(child.nodes[0].metrics.total.is_zero());
    assert_eq!(
        (parent.nodes[0].metrics.rows, parent.nodes[0].metrics.loops),
        (3, 1)
    );
    input.next().unwrap().unwrap();
    input.close().unwrap();
    assert_eq!(parent.nodes[0].metrics.rows, 3);
    drop((input, child, parent));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn admission_failure_closes_the_original_input_and_keeps_cleanup_details() {
    let diagnostics = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(4096);
    let scope = diagnostics.enter_measured(control.clone(), false).unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    let (mut input, calls) = source(3);
    input.fail_close = true;
    let error = match diagnostics.observe(input, "Rejected Scan") {
        Err(e) => e,
        Ok(_) => panic!("admission succeeded"),
    };
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(error.detail().unwrap().contains("scan close failed"));
    assert_eq!(calls.lock().closed, 1);
    assert!(scope.finish_execution().is_err());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn nested_materialized_observers_preserve_borrowed_collection() {
    let diagnostics = QueryDiagnostics::default();
    let scope = diagnostics
        .enter_measured(StorageReadControl::with_limit(65536), true)
        .unwrap();
    let (input, calls) = source(5);
    let input = diagnostics.observe(input, "Native Scan").unwrap();
    let mut input = diagnostics.observe(input, "Pipeline").unwrap();
    let schema = input.row_schema().clone();
    let mut rows = ResultRows::new(schema.columns());
    rows.prepare(schema.columns(), &schema);
    let cancellation = CancellationToken::new();
    let mut sink = MaterializedResultSink::new(&schema, &mut rows, &cancellation);
    input.open().unwrap();
    assert!(input.collect_materialized(&mut sink).unwrap());
    input.close().unwrap();
    assert_eq!(rows.named.len(), 5);
    assert_eq!(calls.lock().borrowed, 5);
    assert!(calls.lock().demands.is_empty());
    let captured = scope.finish_execution().unwrap();
    assert_eq!(captured.nodes.len(), 2);
    for node in captured.nodes.iter() {
        assert_eq!((node.metrics.rows, node.metrics.loops), (5, 1));
        assert!(node.metrics.startup <= node.metrics.total);
    }
}

#[test]
fn retained_worker_inputs_attach_to_each_fetch_request_without_inheriting_old_work() {
    let diagnostics = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65536);
    let empty_request = diagnostics.request_with_control(control.clone());
    let owner = &diagnostics;
    let mut input = std::thread::scope(|threads| {
        threads
            .spawn(move || {
                let _request = QueryDiagnostics::request_scope(Some(empty_request));
                let mut input = owner.observe(source(8).0, "Retained Scan").unwrap();
                input.open().unwrap();
                input.next().unwrap().unwrap();
                input
            })
            .join()
            .unwrap()
    });
    let retained = control.memory().used();
    assert!(retained > 0);
    for timing in [true, false] {
        let scope = diagnostics.enter_measured(control.clone(), timing).unwrap();
        let request = diagnostics.request();
        input = std::thread::scope(|threads| {
            threads
                .spawn(move || {
                    let _request = QueryDiagnostics::request_scope(Some(request));
                    assert_eq!(
                        input
                            .next_with_demand(NonZeroUsize::new(2).unwrap())
                            .unwrap()
                            .unwrap()
                            .rows
                            .len(),
                        2
                    );
                    input
                })
                .join()
                .unwrap()
        });
        let captured = scope.finish_execution().unwrap();
        assert_eq!(captured.nodes.len(), 1);
        assert_eq!(
            (
                captured.nodes[0].metrics.rows,
                captured.nodes[0].metrics.loops
            ),
            (2, 1)
        );
        if !timing {
            assert!(captured.nodes[0].metrics.total.is_zero());
        }
        drop(captured);
        assert_eq!(control.memory().used(), retained);
    }
    input.close().unwrap();
    drop(input);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn simultaneous_session_scopes_do_not_mix_physical_instance_identities() {
    let first = QueryDiagnostics::default();
    let second = QueryDiagnostics::default();
    let a = first
        .enter_measured(StorageReadControl::with_limit(65536), false)
        .unwrap();
    let mut left = first.observe(source(5).0, "First Session").unwrap();
    let b = second
        .enter_measured(StorageReadControl::with_limit(65536), false)
        .unwrap();
    let mut right = second.observe(source(5).0, "Second Session").unwrap();
    left.open().unwrap();
    left.next().unwrap().unwrap();
    right.open().unwrap();
    right
        .next_with_demand(NonZeroUsize::new(2).unwrap())
        .unwrap()
        .unwrap();
    let right_records = b.finish_execution().unwrap();
    let left_records = a.finish_execution().unwrap();
    assert_eq!(left_records.nodes.len(), 1);
    assert_eq!(right_records.nodes.len(), 1);
    assert_eq!(left_records.nodes[0].kind, "First Session");
    assert_eq!(right_records.nodes[0].kind, "Second Session");
    assert_eq!(left_records.nodes[0].metrics.rows, 1);
    assert_eq!(right_records.nodes[0].metrics.rows, 2);
    left.close().unwrap();
    right.close().unwrap();
}

#[test]
fn a_caught_ancestor_admission_failure_cannot_publish_partial_child_diagnostics() {
    let diagnostics = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(4096);
    let parent = diagnostics.enter_measured(control.clone(), false).unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    let child_control = StorageReadControl::with_limit(65536);
    let child = diagnostics
        .enter_measured(child_control.clone(), false)
        .unwrap();
    let (input, calls) = source(3);
    assert!(diagnostics.observe(input, "Rejected Child").is_err());
    assert_eq!(calls.lock().closed, 1);
    assert!(child.finish_execution().is_err());
    assert!(parent.finish_execution().is_err());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(child_control.memory().used(), 0);
}

#[test]
fn cancelled_measurement_admission_preserves_physical_cleanup() {
    let diagnostics = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65536);
    let scope = diagnostics.enter_measured(control.clone(), false).unwrap();
    let (input, calls) = source(3);
    let mut input = diagnostics.observe(input, "Cancelled Scan").unwrap();
    control.cancellation().cancel();
    assert!(input.next().is_err());
    assert!(input.close().is_err());
    assert_eq!(calls.lock().closed, 1);
    assert!(calls.lock().demands.is_empty());
    assert!(scope.finish_execution().is_err());
    drop(input);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn native_projected_aggregation_preserves_borrowed_capabilities_and_row_counts() {
    struct Aggregate(usize);
    impl crate::relational::AggregateExecutor for Aggregate {
        fn consume(&mut self, _: Batch) -> ExecResult<()> {
            panic!("borrowed aggregate must not materialize a batch")
        }
        fn supports_projected_rows(&self) -> bool {
            true
        }
        fn supports_storage_borrowed_rows(&self) -> bool {
            true
        }
        fn consume_projected_row(&mut self, row: &crate::ProjectedRow<'_, '_>) -> ExecResult<()> {
            assert_eq!(row.positional_column(0), Some(&Value::Int(7)));
            self.0 += 1;
            Ok(())
        }
        fn finish(&mut self) -> ExecResult<crate::SpillBuffer> {
            Ok(crate::SpillBuffer::new(1024))
        }
    }
    let diagnostics = QueryDiagnostics::default();
    let scope = diagnostics
        .enter_measured(StorageReadControl::with_limit(65536), true)
        .unwrap();
    let (input, calls) = source(7);
    let input = diagnostics.observe(input, "Scan").unwrap();
    let mut input = diagnostics.observe(input, "Projection").unwrap();
    let mut aggregate = Aggregate(0);
    input.open().unwrap();
    assert!(input.consume_into_aggregate(&mut aggregate).unwrap());
    input.close().unwrap();
    assert_eq!(aggregate.0, 7);
    assert_eq!(calls.lock().borrowed, 7);
    let captured = scope.finish_execution().unwrap();
    assert_eq!(captured.nodes.len(), 2);
    for node in captured.nodes.iter() {
        assert_eq!((node.metrics.rows, node.metrics.loops), (7, 1));
    }
}

#[test]
fn unmeasured_retained_input_admission_uses_its_original_read_owner() {
    let diagnostics = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(4096);
    let _request =
        QueryDiagnostics::request_scope(Some(diagnostics.request_with_control(control.clone())));
    for cancelled in [false, true] {
        let occupied = control.memory().reserve(control.memory().limit()).unwrap();
        if cancelled {
            control.cancellation().cancel();
        }
        let (input, calls) = source(4);
        let error = match diagnostics.observe(input, "Retained Scan") {
            Err(error) => error,
            Ok(_) => panic!("unadmitted input"),
        };
        assert_eq!(
            error.sqlstate(),
            Some(if cancelled { "57014" } else { "53200" })
        );
        assert_eq!(calls.lock().closed, 1);
        assert_eq!(control.memory().used(), occupied.bytes());
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
    }
}
