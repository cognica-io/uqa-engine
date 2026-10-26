//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::diagnostics::QueryDiagnostics;
use std::io::{Read, Seek, Write};
use uqa_core::io_usage::IOOptions;
use uqa_storage::{read_control::StorageReadControl, temporary_file::BlockTemporaryFile};

fn input(blocks: usize) -> BlockTemporaryFile<4> {
    let mut file = BlockTemporaryFile::new().unwrap();
    for _ in 0..blocks {
        file.write_all(b"read").unwrap();
    }
    file.rewind().unwrap();
    file
}

fn read(file: &mut BlockTemporaryFile<4>) {
    let mut bytes = [0; 4];
    file.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"read");
}

fn options() -> IOOptions {
    IOOptions {
        buffers: true,
        wal: true,
    }
}

#[test]
fn plan_io_excludes_destinations_but_enclosing_invocations_include_all_work() {
    let mut source = input(6);
    let mut destination = BlockTemporaryFile::<4>::new().unwrap();
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let scope = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let measurement = QueryMeasurement::new(false, control.memory());
    let mut serialized = IOUsage::default();
    for _ in 0..2 {
        measurement
            .run(|| {
                for _ in 0..3 {
                    read(&mut source);
                    measurement.output(1, || {
                        let operation =
                            Operation::enter(&IOContext::current(), control.memory())?.unwrap();
                        destination.write_all(b"sink").unwrap();
                        serialized = serialized.checked_add(operation.finish()?).unwrap();
                        Ok(())
                    })?;
                }
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(
        (measurement.metrics().rows, measurement.metrics().loops),
        (6, 2)
    );
    assert_eq!(
        (
            measurement.io().temporary.reads,
            measurement.io().temporary.written
        ),
        (6, 0)
    );
    assert_eq!(
        (serialized.temporary.reads, serialized.temporary.written),
        (0, 6)
    );
    drop(measurement);
    let captured = scope.finish_execution().unwrap();
    assert_eq!(
        (captured.io.temporary.reads, captured.io.temporary.written),
        (6, 6)
    );
    drop(captured);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn nested_plan_excludes_its_output_while_the_calling_plan_retains_it() {
    let mut source = input(2);
    let mut destination = BlockTemporaryFile::<4>::new().unwrap();
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let scope = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let outer = QueryMeasurement::new(false, control.memory());
    outer
        .run(|| {
            read(&mut source);
            let nested = QueryMeasurement::new(false, control.memory());
            nested.run(|| {
                read(&mut source);
                nested.output(1, || {
                    destination.write_all(b"sink").unwrap();
                    Ok(())
                })
            })?;
            assert_eq!(
                (nested.io().temporary.reads, nested.io().temporary.written),
                (1, 0)
            );
            outer.output(1, || {
                destination.write_all(b"sink").unwrap();
                Ok(())
            })
        })
        .unwrap();
    assert_eq!(
        (outer.io().temporary.reads, outer.io().temporary.written),
        (2, 1)
    );
    drop(outer);
    let captured = scope.finish_execution().unwrap();
    assert_eq!(
        (captured.io.temporary.reads, captured.io.temporary.written),
        (2, 2)
    );
    drop(captured);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn root_io_admission_precedes_execution_and_error_or_unwind_releases_the_owner() {
    let mut source = input(3);
    let owner = QueryDiagnostics::default();
    let control = StorageReadControl::with_limit(65_536);
    let scope = owner
        .enter_measured(control.clone(), false, options())
        .unwrap();
    let rejected = QueryMeasurement::new(false, &MemoryBudget::new(0));
    assert_eq!(
        rejected
            .run::<()>(|| panic!("unadmitted plan executed"))
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(rejected.io(), IOUsage::default());
    assert_eq!(rejected.metrics().loops, 0);
    drop(rejected);
    let baseline = control.memory().used();
    let measurement = QueryMeasurement::new(false, control.memory());
    let error = measurement
        .run(|| {
            read(&mut source);
            measurement.output::<()>(1, || {
                Err(SQLError::Routine {
                    sqlstate: "22012".into(),
                    message: "destination failure".into(),
                })
            })
        })
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(measurement.io().temporary.reads, 1);
    assert_eq!(control.memory().used(), baseline);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = measurement.run::<()>(|| {
            read(&mut source);
            measurement.output::<()>(1, || panic!("destination panic"))
        });
    }));
    assert!(panic.is_err());
    assert_eq!(measurement.io().temporary.reads, 1);
    assert_eq!(control.memory().used(), baseline);
    measurement
        .run(|| {
            read(&mut source);
            Ok(())
        })
        .unwrap();
    assert_eq!(measurement.io().temporary.reads, 2);
    drop(measurement);
    let captured = scope.finish_execution().unwrap();
    assert_eq!(captured.io.temporary.reads, 3);
    drop(captured);
    assert_eq!(control.memory().used(), 0);
}
