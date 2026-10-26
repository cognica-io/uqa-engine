//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    query::diagnostics::QueryDiagnostics, relational::AggregateExecutor, Batch, PhysicalOperator,
    PhysicalRow, RowSchema,
};
use std::io::{Read, Seek, SeekFrom, Write};
use uqa_core::{io_usage::IOOptions, Value};
use uqa_storage::temporary_file::BlockTemporaryFile;

fn options() -> IOOptions {
    IOOptions {
        buffers: true,
        wal: true,
    }
}

#[test]
fn closed_nested_capture_cannot_record_or_fail_an_active_parent() {
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let outer = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let inner = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let retained = owner.capture().unwrap();
    let id = owner.allocate_instance().unwrap();
    retained.register_execution(id, "Closed Child").unwrap();
    drop(inner.finish_execution().unwrap());
    assert!(!retained.has_execution_scope());
    let mut step = Step::new(None);
    step.ready(7).unwrap();
    retained
        .record_execution(
            id,
            Event::Consume(step.finish().unwrap()),
            IOUsage {
                temporary: uqa_core::io_usage::BlockUsage {
                    reads: 11,
                    ..uqa_core::io_usage::BlockUsage::default()
                },
                ..IOUsage::default()
            },
        )
        .unwrap();
    retained.fail_execution(Failure::Measurement);
    let captured = outer.finish_execution().unwrap();
    assert_eq!(captured.nodes.len(), 1);
    assert_eq!(captured.nodes[0].metrics.rows, 0);
    assert_eq!(captured.nodes[0].io, IOUsage::default());
    drop((captured, retained));
    assert_eq!(control.memory().used(), 0);
}

struct Reader {
    file: BlockTemporaryFile<4>,
    schema: RowSchema,
    count: usize,
    position: usize,
    panic_after_read: bool,
}

fn reader(count: usize) -> Reader {
    let mut file = BlockTemporaryFile::new().unwrap();
    for _ in 0..count {
        file.write_all(b"abcd").unwrap();
    }
    file.rewind().unwrap();
    Reader {
        file,
        schema: RowSchema::new(vec!["value".into()]),
        count,
        position: 0,
        panic_after_read: false,
    }
}

impl PhysicalOperator for Reader {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }
    fn estimated_cardinality(&self) -> Option<u64> {
        Some(self.count as u64)
    }
    fn open(&mut self) -> ExecResult<()> {
        Ok(())
    }
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self.position == self.count {
            return Ok(None);
        }
        let mut bytes = [0; 4];
        self.file.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"abcd");
        self.position += 1;
        assert!(
            !self.panic_after_read,
            "abort observed input after physical I/O"
        );
        Ok(Some(Batch::from_physical_rows(
            self.schema.clone(),
            vec![PhysicalRow::from_values(vec![Value::Int(1)])],
        )))
    }
    fn rewind(&mut self) -> ExecResult<()> {
        self.file.seek(SeekFrom::Start(0)).unwrap();
        self.position = 0;
        Ok(())
    }
    fn close(&mut self) -> ExecResult<()> {
        Ok(())
    }
    fn consume_into_aggregate(&mut self, receiver: &mut dyn AggregateExecutor) -> ExecResult<bool> {
        while let Some(batch) = self.next()? {
            receiver.consume(batch)?;
        }
        Ok(true)
    }
}

#[test]
fn nested_physical_instances_include_upstream_blocks_once_per_loop() {
    let input = reader(3);
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let scope = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let input = owner.observe(Box::new(input), "Reader").unwrap();
    let mut input = owner.observe(input, "Pipeline").unwrap();
    input.open().unwrap();
    for cycle in 0..2 {
        if cycle != 0 {
            input.rewind().unwrap();
        }
        while input.next().unwrap().is_some() {}
    }
    input.close().unwrap();
    let captured = scope.finish_execution().unwrap();
    assert_eq!(captured.io.temporary.reads, 6);
    assert_eq!(captured.nodes.len(), 2);
    for node in captured.nodes.iter() {
        assert_eq!(node.io.temporary.reads, 6);
        assert_eq!(node.io.temporary.written, 0);
        assert_eq!((node.metrics.rows, node.metrics.loops), (6, 2));
        assert!(node.metrics.total.is_zero());
    }
    drop((input, captured));
    assert_eq!(control.memory().used(), 0);
}

struct WritingAggregate(BlockTemporaryFile<4>);
impl AggregateExecutor for WritingAggregate {
    fn consume(&mut self, batch: Batch) -> ExecResult<()> {
        for _ in &batch.rows {
            self.0.write_all(b"sink").unwrap();
        }
        Ok(())
    }
    fn finish(&mut self) -> ExecResult<crate::spill::SpillBuffer> {
        unreachable!("only consume is invoked")
    }
}

#[test]
fn aggregate_handoffs_exclude_downstream_blocks_from_every_source_instance() {
    let input = reader(5);
    let mut aggregate = WritingAggregate(BlockTemporaryFile::new().unwrap());
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let scope = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let input = owner.observe(Box::new(input), "Reader").unwrap();
    let mut input = owner.observe(input, "Pipeline").unwrap();
    input.open().unwrap();
    assert!(input.consume_into_aggregate(&mut aggregate).unwrap());
    input.close().unwrap();
    let captured = scope.finish_execution().unwrap();
    assert_eq!(captured.io.temporary.reads, 5);
    assert_eq!(captured.io.temporary.written, 5);
    for node in captured.nodes.iter() {
        assert_eq!(node.io.temporary.reads, 5);
        assert_eq!(node.io.temporary.written, 0);
        assert_eq!(node.metrics.rows, 5);
    }
}

#[test]
fn nested_invocation_options_do_not_hide_ancestor_node_measurements() {
    let input = reader(3);
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let outer = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let mut input = owner.observe(Box::new(input), "Reader").unwrap();
    let inner = owner
        .enter_measured(control.clone(), false, IOOptions::default())
        .unwrap();
    input.next().unwrap().unwrap();
    let inner = inner.finish_execution().unwrap();
    assert_eq!(inner.io, IOUsage::default());
    input.next().unwrap().unwrap();
    let outer = outer.finish_execution().unwrap();
    assert_eq!(outer.io.temporary.reads, 2);
    assert_eq!(outer.nodes[0].io.temporary.reads, 2);
    input.next().unwrap().unwrap();
    assert_eq!(outer.nodes[0].io.temporary.reads, 2);
    drop((input, inner, outer));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn parallel_instances_have_separate_totals_and_a_single_inclusive_root() {
    let inputs: Vec<_> = (1..=4).map(reader).collect();
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let scope = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let captured = owner.capture().unwrap();
    let worker = owner.fork();
    std::thread::scope(|threads| {
        for input in inputs {
            let worker = &worker;
            let captured = &captured;
            threads.spawn(move || {
                let _scope = worker.bind(captured).unwrap();
                let mut input = worker.observe(Box::new(input), "Parallel Reader").unwrap();
                input.open().unwrap();
                while input.next().unwrap().is_some() {}
                input.close().unwrap();
            });
        }
    });
    let actual = scope.finish_execution().unwrap();
    assert_eq!(actual.io.temporary.reads, 10);
    let mut counts: Vec<_> = actual
        .nodes
        .iter()
        .map(|node| node.io.temporary.reads)
        .collect();
    counts.sort_unstable();
    assert_eq!(counts, [1, 2, 3, 4]);
    drop((actual, worker, captured));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn retained_reader_collects_only_the_current_fetch_request() {
    let input = reader(6);
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let request = owner.request_with_control(control.clone());
    std::thread::scope(|threads| {
        threads
            .spawn(|| {
                let requests = QueryDiagnostics::request_scope(Some(request));
                let mut input = owner.observe(Box::new(input), "Retained Reader").unwrap();
                input.open().unwrap();
                input.next().unwrap().unwrap();
                for _ in 0..2 {
                    let scope = owner
                        .enter_measured(control.clone(), false, options())
                        .unwrap();
                    requests.replace(Some(owner.request()));
                    input.next().unwrap().unwrap();
                    let actual = scope.finish_execution().unwrap();
                    assert_eq!(actual.io.temporary.reads, 1);
                    assert_eq!(actual.nodes[0].io.temporary.reads, 1);
                    requests.replace(None);
                    input.next().unwrap().unwrap();
                }
                input.close().unwrap();
            })
            .join()
            .unwrap();
    });
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn operation_admission_fails_before_reading_and_closed_captures_release_records() {
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let empty = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let retained = owner.capture().unwrap();
    drop(empty.finish_execution().unwrap());
    let closed_owner_bytes = control.memory().used();
    drop(retained);
    assert_eq!(control.memory().used(), 0);
    for finish in 0..4 {
        let mut input = reader(1);
        input.panic_after_read = finish == 2;
        let scope = owner
            .enter_measured(control.clone(), false, options())
            .unwrap();
        let retained = owner.capture().unwrap();
        let mut input = owner.observe(Box::new(input), "Reader").unwrap();
        input.open().unwrap();
        assert!(control.memory().used() > closed_owner_bytes);
        match finish {
            0 => {
                input.next().unwrap().unwrap();
                drop(input);
                drop(scope.finish_execution().unwrap());
            }
            1 => {
                control.cancellation().cancel();
                drop(input);
                assert!(scope.finish_execution().is_err());
                control.cancellation().reset();
            }
            2 => {
                let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _scope = scope;
                    let mut input = input;
                    input.next().unwrap();
                }));
                assert!(error.is_err());
                assert_eq!(
                    retained
                        .io
                        .as_ref()
                        .unwrap()
                        .snapshot()
                        .unwrap()
                        .temporary
                        .reads,
                    1
                );
            }
            3 => {
                let occupied = control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap();
                let error = input.next().unwrap_err();
                assert_eq!(physical_exec_error(error).sqlstate(), Some("53200"));
                assert_eq!(
                    retained
                        .io
                        .as_ref()
                        .unwrap()
                        .snapshot()
                        .unwrap()
                        .temporary
                        .reads,
                    0
                );
                drop((occupied, input));
                assert!(scope.finish_execution().is_err());
            }
            _ => unreachable!(),
        }
        assert_eq!(
            control.memory().used(),
            closed_owner_bytes,
            "completion mode {finish}"
        );
        let records = retained.records.lock();
        assert!(records.closed && records.executions.is_none() && records.searches.is_empty());
        drop(records);
        drop(retained);
        assert_eq!(control.memory().used(), 0);
    }
}
