//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::copy::compile_copy;
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn options(format: &str) -> CopyOptions {
    compile_copy(&format!("COPY t TO STDOUT WITH (FORMAT {format})"))
        .unwrap()
        .options
}

#[test]
fn text_and_csv_bytes_match_postgresql_typed_output() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("pg18-output.json")).unwrap();
    let columns = ["n", "empty", "special", "bytes", "flag", "padded", "ids"]
        .map(str::to_owned)
        .to_vec();
    let values = vec![
        Value::Null,
        Value::Str(String::new()),
        Value::Str("é,\t\"\\\n".into()),
        Value::Bytes(vec![0, 255]),
        Value::Bool(true),
        Value::FixedChar("x  ".into()),
        Value::List(vec![Value::Int(1), Value::Int(2)]),
    ];
    let mut types = vec![None; values.len()];
    types[6] = Some(ColumnType::Int2Vector);
    let result = SQLResult::from_typed_rows_with_positions(
        columns,
        types,
        vec![BTreeMap::new()],
        Some(vec![values]),
    );
    for case in fixture["cases"].as_array().unwrap() {
        let options = compile_copy(case["sql"].as_str().unwrap()).unwrap().options;
        let mut output = Vec::new();
        write_copy_result(
            &result,
            &options,
            &mut output,
            None,
            &ProductionControl::uncontrolled(),
        )
        .unwrap();
        assert_eq!(output, case["expected"].as_str().unwrap().as_bytes());
        assert_eq!(
            crate::copy::encode_copy_result(&result, &options).unwrap(),
            output
        );
    }
}

struct DigestSink<'a> {
    budget: &'a MemoryBudget,
    hash: crc32fast::Hasher,
    bytes: usize,
    peak: usize,
}

impl Write for DigestSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        assert!(bytes.len() <= WRITE_BYTES);
        self.peak = self.peak.max(self.budget.used());
        self.hash.update(bytes);
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        panic!("COPY does not own sink flushing")
    }
}

#[test]
fn encoded_export_larger_than_allowance_retains_only_current_field() {
    let budget = MemoryBudget::new(16 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let options = options("text");
    let value = Value::Bytes(vec![0xab; 4096]);
    let mut sink = DigestSink {
        budget: &budget,
        hash: crc32fast::Hasher::new(),
        bytes: 0,
        peak: 0,
    };
    let mut expected = crc32fast::Hasher::new();
    let expected_row = format!("\\\\x{}\n", "ab".repeat(4096));
    {
        let mut encoder = CopyEncoder::new(&options, &mut sink, None, control).unwrap();
        for _ in 0..1024 {
            encoder.write_row([(Some(&value), None)]).unwrap();
            assert_eq!(budget.used(), 0);
            expected.update(expected_row.as_bytes());
        }
    }
    assert_eq!(sink.bytes, expected_row.len() * 1024);
    assert_eq!(sink.hash.finalize(), expected.finalize());
    assert!(sink.peak >= 8194);
    assert!(budget.peak() <= 16 * 1024);
    assert_eq!(budget.used(), 0);
}

#[test]
fn large_borrowed_unicode_field_needs_no_owned_scratch() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let options = options("csv");
    let text = "한글,é\"\n".repeat(32 * 1024);
    let expected = format!("\"{}\"\n", text.replace('"', "\"\""));
    let value = Value::Str(text);
    let mut sink = DigestSink {
        budget: &budget,
        hash: crc32fast::Hasher::new(),
        bytes: 0,
        peak: 0,
    };
    CopyEncoder::new(&options, &mut sink, None, control)
        .unwrap()
        .write_row([(Some(&value), None)])
        .unwrap();
    assert_eq!(sink.bytes, expected.len());
    assert_eq!(sink.hash.finalize(), crc32fast::hash(expected.as_bytes()));
    assert_eq!(budget.peak(), 0);
}

#[test]
fn partial_and_interrupted_writes_preserve_bytes() {
    struct ShortSink {
        output: Vec<u8>,
        interrupts: usize,
    }
    impl Write for ShortSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.interrupts > 0 {
                self.interrupts -= 1;
                return Err(ErrorKind::Interrupted.into());
            }
            let count = bytes.len().min(3);
            self.output.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let options = options("csv");
    let value = Value::Str("é,\"hello\"\n".into());
    let mut sink = ShortSink {
        output: Vec::new(),
        interrupts: 3,
    };
    CopyEncoder::new(&options, &mut sink, None, ProductionControl::uncontrolled())
        .unwrap()
        .write_row([(Some(&value), None)])
        .unwrap();
    assert_eq!(sink.output, "\"é,\"\"hello\"\"\n\"\n".as_bytes());
}

#[test]
fn cancellation_after_sink_write_releases_formatted_scratch() {
    struct CancellingSink<'a> {
        token: &'a CancellationToken,
        calls: usize,
        interrupt: bool,
    }
    impl Write for CancellingSink<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.token.cancel();
            if self.interrupt {
                Err(ErrorKind::Interrupted.into())
            } else {
                Ok(bytes.len())
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for cancel_original in [false, true] {
        for interrupt in [false, true] {
            let budget = MemoryBudget::new(16 * 1024);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let mut sink = CancellingSink {
                token: if cancel_original {
                    &original
                } else {
                    &invoking
                },
                calls: 0,
                interrupt,
            };
            let value = Value::Bytes(vec![0xab; 4096]);
            let options = options("text");
            let error = CopyEncoder::new(&options, &mut sink, None, control)
                .unwrap()
                .write_row([(Some(&value), None)])
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(sink.calls, 1);
            assert!(budget.peak() > 0);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn sink_failure_and_write_zero_release_scratch_without_retrying() {
    struct FailingSink {
        calls: usize,
        bytes: usize,
        zero: bool,
    }
    impl Write for FailingSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 1 {
                self.bytes += bytes.len();
                return Ok(bytes.len());
            }
            if self.zero {
                Ok(0)
            } else {
                Err(std::io::Error::other("closed export sink"))
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for zero in [false, true] {
        let budget = MemoryBudget::new(16 * 1024);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let options = options("text");
        let value = Value::Bytes(vec![0xab; 4096]);
        let mut sink = FailingSink {
            calls: 0,
            bytes: 0,
            zero,
        };
        let error = CopyEncoder::new(&options, &mut sink, None, control)
            .unwrap()
            .write_row([(Some(&value), None)])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("58030"));
        if !zero {
            assert!(error.to_string().contains("closed export sink"));
        }
        assert_eq!(sink.calls, 2);
        assert_eq!(sink.bytes, 2);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn scratch_refusal_and_unsupported_format_emit_no_bytes() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let options = options("text");
    let value = Value::Bytes(vec![0xab; 16]);
    let mut output = Vec::new();
    let error = CopyEncoder::new(&options, &mut output, None, control)
        .unwrap()
        .write_row([(Some(&value), None)])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert!(output.is_empty());
    assert_eq!(budget.used(), 0);
    let mut binary = options;
    binary.format = CopyFormat::Binary;
    let error = CopyEncoder::new(&binary, &mut output, None, control)
        .err()
        .unwrap();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert!(output.is_empty());
}

#[test]
fn custom_csv_options_preserve_null_quotes_and_escape_bytes() {
    // Independently captured with PostgreSQL 18.4, including a quoted NULL marker and a distinct escape character.
    let options = compile_copy(
        "COPY t TO STDOUT WITH (FORMAT csv, DELIMITER '|', NULL 'nil', QUOTE '~', ESCAPE '!')",
    )
    .unwrap()
    .options;
    let values = [
        Value::Null,
        Value::Str("nil".into()),
        Value::Str("a|b~c!".into()),
        Value::Str(String::new()),
    ];
    let mut output = Vec::new();
    CopyEncoder::new(
        &options,
        &mut output,
        None,
        ProductionControl::uncontrolled(),
    )
    .unwrap()
    .write_row(values.iter().map(|value| (Some(value), None)))
    .unwrap();
    assert_eq!(output, b"nil|~nil~|~a|b!~c!!~|\n");
}

#[test]
fn interval_output_reads_live_settings_and_restores_its_caller() {
    use uqa_core::{TemporalIntervalStyle, TemporalValue};
    struct Session(std::cell::Cell<TemporalIntervalStyle>);
    impl EngineHook for Session {
        fn nextval(&self, _: &str) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn currval(&self, _: &str) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn temporal_interval_style(&self) -> Result<Option<TemporalIntervalStyle>, SQLError> {
            Ok(Some(self.0.get()))
        }
    }
    let _scope = crate::expr::IntervalStyleScope::enter(TemporalIntervalStyle::Postgres);
    let session = Session(std::cell::Cell::new(TemporalIntervalStyle::Iso8601));
    let budget = MemoryBudget::new(1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let value = Value::Temporal(TemporalValue::Interval {
        months: 0,
        days: 1,
        micros: 7_200_000_000,
    });
    let options = options("text");
    let mut output = Vec::new();
    let mut encoder = CopyEncoder::new(&options, &mut output, Some(&session), control).unwrap();
    encoder
        .write_row([(Some(&value), Some(&ColumnType::Interval))])
        .unwrap();
    session.0.set(TemporalIntervalStyle::PostgresVerbose);
    encoder
        .write_row([(Some(&value), Some(&ColumnType::Interval))])
        .unwrap();
    assert_eq!(output, b"P1DT2H\n@ 1 day 2 hours\n");
    assert_eq!(
        crate::expr::temporal_interval_style(),
        TemporalIntervalStyle::Postgres
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn csv_single_column_end_marker_matches_postgresql_quoting() {
    let options = options("csv");
    let marker = Value::Str(r"\.".into());
    let second = Value::Int(1);
    let mut output = Vec::new();
    let mut encoder = CopyEncoder::new(
        &options,
        &mut output,
        None,
        ProductionControl::uncontrolled(),
    )
    .unwrap();
    encoder.write_row([(Some(&marker), None)]).unwrap();
    encoder
        .write_row([(Some(&marker), None), (Some(&second), None)])
        .unwrap();
    // PostgreSQL 18.4 quotes a lone marker; a multi-column row cannot be an end marker.
    assert_eq!(output, b"\"\\.\"\n\\.,1\n");
}

#[test]
fn positional_duplicate_labels_and_zero_column_rows_are_preserved() {
    let options = options("text");
    let result = SQLResult::from_rows_with_positions(
        vec!["x".into(), "x".into()],
        vec![BTreeMap::new()],
        Some(vec![vec![Value::Int(1), Value::Int(2)]]),
    );
    let mut output = Vec::new();
    write_copy_result(
        &result,
        &options,
        &mut output,
        None,
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    assert_eq!(output, b"1\t2\n");
    let result = SQLResult::from_rows(Vec::new(), vec![BTreeMap::new(), BTreeMap::new()]);
    output.clear();
    write_copy_result(
        &result,
        &options,
        &mut output,
        None,
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    assert_eq!(output, b"\n\n");
}
