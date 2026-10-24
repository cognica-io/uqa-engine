//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{query::output::QueryOutput, Batch, PhysicalRow, RowSchema, SpillBuffer};
use sha2::{Digest, Sha256};

#[test]
fn copy_input_checks_cancellation_before_read_and_after_data_or_eof() {
    struct Source {
        token: CancellationToken,
        reads: usize,
        count: usize,
    }
    impl Read for Source {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            buffer[0] = b'1';
            self.token.cancel();
            Ok(self.count)
        }
    }
    for count in [0, 1] {
        let token = CancellationToken::new();
        let mut input = Source {
            token: token.clone(),
            reads: 0,
            count,
        };
        assert_eq!(
            read_copy_input(&mut input, &token).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(input.reads, 1);
        assert_eq!(
            read_copy_input(&mut input, &token).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(input.reads, 1);
    }
}

#[test]
fn copy_input_preserves_partial_reads_interruption_and_source_errors() {
    struct Source {
        reads: usize,
        fail: bool,
    }
    impl Read for Source {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            match self.reads {
                1 => Err(std::io::ErrorKind::Interrupted.into()),
                2 => {
                    buffer[..2].copy_from_slice(b"1\n");
                    Ok(2)
                }
                _ if self.fail => Err(std::io::Error::other("original source failure")),
                _ => Ok(0),
            }
        }
    }
    let token = CancellationToken::new();
    assert_eq!(
        read_copy_input(
            &mut Source {
                reads: 0,
                fail: false
            },
            &token
        )
        .unwrap(),
        b"1\n"
    );
    let error = read_copy_input(
        &mut Source {
            reads: 0,
            fail: true,
        },
        &token,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("58030"));
    assert!(error.to_string().contains("original source failure"));
}

struct Sink {
    digest: Sha256,
    bytes: usize,
    flushes: usize,
    fail_after: Option<usize>,
}

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.fail_after.is_some_and(|limit| self.bytes >= limit) {
            return Err(std::io::Error::other("closed spill output"));
        }
        self.digest.update(bytes);
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

fn spool(directory: &std::path::Path, rows: usize, width: usize) -> QueryOutput {
    let schema = RowSchema::with_types(vec!["body".into()], vec![Some(uqa_sql::ColumnType::Text)]);
    let mut buffer = SpillBuffer::new_in(64 * 1024, directory);
    for _ in 0..rows {
        buffer
            .push(Batch::from_physical_rows(
                schema.clone(),
                vec![PhysicalRow::from_values(vec![Value::Str(
                    "x".repeat(width),
                )])],
            ))
            .unwrap();
    }
    let rows = buffer.into_shared(schema.clone()).unwrap();
    assert!(rows.has_spilled());
    QueryOutput {
        columns: schema.columns().to_vec(),
        column_types: schema.column_types().to_vec(),
        internal_schema: schema,
        rows: QueryRows::SharedSpill(rows),
    }
}

#[test]
fn copy_spill_delivery_does_not_rematerialize_the_complete_result() {
    let directory = tempfile::tempdir().unwrap();
    let rows = 2048;
    let width = 16 * 1024;
    let output = spool(directory.path(), rows, width);
    let options = compile_copy("COPY (SELECT 1) TO STDOUT").unwrap().options;
    let memory = MemoryBudget::new(64 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut sink = Sink {
        digest: Sha256::new(),
        bytes: 0,
        flushes: 0,
        fail_after: None,
    };
    let allocations = allocation_counter::measure(|| {
        assert_eq!(
            write_statement_output(
                StatementOutput::Query(output),
                &options,
                &mut sink,
                None,
                &control
            )
            .unwrap(),
            rows as u64
        );
    });
    // One decoded batch, its schema and formatter scratch may coexist; the 32MiB complete result may not.
    assert!(allocations.bytes_max < 512 * 1024, "{allocations:?}");
    assert_eq!(memory.used(), 0);
    assert_eq!(sink.bytes, rows * (width + 1));
    assert_eq!(sink.flushes, 1);
    let row = format!("{}\n", "x".repeat(width));
    let mut expected = Sha256::new();
    for _ in 0..rows {
        expected.update(row.as_bytes());
    }
    assert_eq!(sink.digest.finalize(), expected.finalize());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn copy_spill_failure_and_cancellation_release_the_spool() {
    for cancel in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let output = spool(directory.path(), 128, 4096);
        let options = compile_copy("COPY (SELECT 1) TO STDOUT").unwrap().options;
        let memory = MemoryBudget::new(64 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut sink = Sink {
            digest: Sha256::new(),
            bytes: 0,
            flushes: 0,
            fail_after: Some(4097),
        };
        if cancel {
            token.cancel();
        }
        let error = write_statement_output(
            StatementOutput::Query(output),
            &options,
            &mut sink,
            None,
            &control,
        )
        .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if cancel { "57014" } else { "58030" })
        );
        assert_eq!(sink.bytes, if cancel { 0 } else { 4097 });
        assert_eq!(sink.flushes, 0);
        assert_eq!(memory.used(), 0);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
