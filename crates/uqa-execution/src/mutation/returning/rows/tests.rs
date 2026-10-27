//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uqa_core::Value;

#[test]
fn returning_rows_spill_without_retaining_complete_output() {
    struct Sink {
        digest: Sha256,
        bytes: usize,
        flushes: usize,
    }
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.digest.update(bytes);
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let token = CancellationToken::new();
    let mut rows = ReturningRows::with_budget(64 * 1024, token);
    rows.buffer = SpillBuffer::new_in(32 * 1024, directory.path());
    let schema = RowSchema::new(vec!["item".into(), "item".into()]);
    let mut sink = Sink {
        digest: Sha256::new(),
        bytes: 0,
        flushes: 0,
    };
    let options = uqa_sql::copy::compile_copy("COPY (SELECT 1) TO STDOUT")
        .unwrap()
        .options;
    let memory = uqa_core::memory::MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = uqa_core::memory::ProductionControl::new(&memory, &token, &token);
    let allocations = allocation_counter::measure(|| {
        for index in 0..2048 {
            rows.push(OwnedPhysicalRow::new(
                schema.clone(),
                PhysicalRow::from_values(vec![Value::Int(index), Value::Str("x".repeat(16384))]),
            ))
            .unwrap();
        }
        assert_eq!(rows.len(), 2048);
        assert!(rows.buffer.has_spilled());
        let declared = RowSchema::with_types(
            schema.columns().to_vec(),
            vec![
                Some(uqa_sql::ColumnType::Integer),
                Some(uqa_sql::ColumnType::Text),
            ],
        );
        let result =
            crate::mutation::output::MutationOutput::from_returning(declared, rows, 2048).unwrap();
        assert_eq!(
            crate::copy::write_statement_output(
                crate::statement::plan_executor::StatementOutput::Mutation(result),
                &options,
                &mut sink,
                None,
                &control
            )
            .unwrap(),
            2048
        );
    });
    let mut expected = Sha256::new();
    let mut expected_bytes = 0;
    for index in 0..2048 {
        let row = format!("{index}\t{}\n", "x".repeat(16384));
        expected_bytes += row.len();
        expected.update(row.as_bytes());
    }
    assert_eq!(sink.digest.finalize(), expected.finalize());
    assert_eq!(sink.bytes, expected_bytes);
    assert_eq!(sink.flushes, 1);
    assert_eq!(memory.used(), 0);
    // Production and COPY delivery share the bound; neither complete physical nor public result rows may coexist with the 32 MiB export.
    assert!(allocations.bytes_max < 1024 * 1024, "{allocations:?}");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn returning_rows_detach_unselected_source_fragments() {
    let mut rows = ReturningRows::with_budget(64 * 1024, CancellationToken::new());
    let schema = RowSchema::new(vec!["id".into()]);
    let allocations = allocation_counter::measure(|| {
        for index in 0..64 {
            let source = Arc::new(vec![
                Value::Str("private".repeat(150_000)),
                Value::Int(index),
            ]);
            rows.push(OwnedPhysicalRow::new(
                schema.clone(),
                PhysicalRow::from_shared_values(source, Arc::from([1])),
            ))
            .unwrap();
        }
        assert_eq!(rows.len(), 64);
        for (index, row) in rows.into_rows().unwrap().enumerate() {
            assert_eq!(
                row.unwrap().view().value_at(0),
                Some(&Value::Int(index as i64))
            );
        }
    });
    // At most the current 1.05 MiB source and bounded selected IDs coexist, not 64 retained source documents.
    assert!(allocations.bytes_max < 3 * 1024 * 1024, "{allocations:?}");
}

#[test]
fn returning_rows_release_spills_after_cancellation_and_write_failure() {
    let directory = tempfile::tempdir().unwrap();
    let token = CancellationToken::new();
    let schema = RowSchema::new(vec!["body".into()]);
    let row = || {
        OwnedPhysicalRow::new(
            schema.clone(),
            PhysicalRow::from_values(vec![Value::Str("x".repeat(4096))]),
        )
    };
    let mut rows = ReturningRows::with_budget(1, token.clone());
    rows.buffer = SpillBuffer::new_in(0, directory.path());
    rows.push(row()).unwrap();
    let mut reader = rows.into_rows().unwrap();
    token.cancel();
    assert_eq!(
        reader.next().unwrap().unwrap_err().sqlstate(),
        Some("57014")
    );
    assert!(reader.next().is_none());
    drop(reader);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);

    let buffered_token = CancellationToken::new();
    let mut buffered = ReturningRows::with_budget(64 * 1024, buffered_token.clone());
    buffered.push(row()).unwrap();
    buffered.push(row()).unwrap();
    let mut buffered = buffered.into_rows().unwrap();
    assert!(buffered.next().unwrap().is_ok());
    buffered_token.cancel();
    assert_eq!(
        buffered.next().unwrap().unwrap_err().sqlstate(),
        Some("57014")
    );
    assert!(buffered.next().is_none());
    drop(buffered);

    let invalid = directory.path().join("not-a-directory");
    std::fs::write(&invalid, b"preserve").unwrap();
    let mut rows = ReturningRows::with_budget(1, CancellationToken::new());
    rows.buffer = SpillBuffer::new_in(0, &invalid);
    assert!(rows.push(row()).is_err());
    drop(rows);
    assert_eq!(std::fs::read(&invalid).unwrap(), b"preserve");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn immediate_returning_delivery_retains_no_rows_and_propagates_stop_or_error() {
    use crate::query::consumer::{QueryConsumerControl, QueryRowConsumer};
    use std::cell::Cell;
    struct Receiver {
        calls: Cell<usize>,
        fail: bool,
    }
    impl QueryRowConsumer for Receiver {
        fn begin(&self, _: &[String], _: &RowSchema) -> Result<(), SQLError> {
            Ok(())
        }
        fn consume(&self, row: OwnedPhysicalRow) -> Result<QueryConsumerControl, SQLError> {
            self.calls.set(self.calls.get() + 1);
            assert_eq!(row.schema.column_type(0), Some(&uqa_sql::ColumnType::Text));
            if self.fail {
                Err(SQLError::Routine {
                    sqlstate: "22000".into(),
                    message: "receiver failure".into(),
                })
            } else {
                Ok(QueryConsumerControl::Stop)
            }
        }
    }
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(uqa_sql::ColumnType::Text)]);
    for fail in [false, true] {
        let receiver = Receiver {
            calls: Cell::new(0),
            fail,
        };
        let destination =
            crate::mutation::returning::ReturningDestination::new(&receiver, &schema).unwrap();
        let mut rows = ReturningRows::default();
        let allocation = allocation_counter::measure(|| {
            for _ in 0..64 {
                let row = OwnedPhysicalRow::new(
                    schema.clone(),
                    PhysicalRow::from_values(vec![Value::Str("x".repeat(1024 * 1024))]),
                );
                let result = rows.push_to(row, Some(destination));
                if fail {
                    assert_eq!(result.unwrap_err().sqlstate(), Some("22000"));
                    break;
                }
                result.unwrap();
            }
        });
        assert_eq!(receiver.calls.get(), 1);
        assert!(rows.is_empty());
        assert!(rows.into_batches().unwrap().next().is_none());
        assert!(allocation.bytes_max < 3 * 1024 * 1024, "{allocation:?}");
    }
}
