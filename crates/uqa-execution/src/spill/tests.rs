//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use uqa_core::{DecimalValue, TemporalValue, Value};

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
        .write_all(&1_u64.to_le_bytes())
        .unwrap();
    buffer
        .spill_file
        .as_mut()
        .unwrap()
        .as_file_mut()
        .write_all(&[0xff])
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
    assert!(error.to_string().contains("truncated schema column count"));
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
    file.write_all(&[1, 2, 3, 4]).unwrap();
    file.flush().unwrap();

    let mut reader = buffer.reader().unwrap();
    assert!(reader.next().unwrap().is_ok());
    let error = reader.next().unwrap().unwrap_err();
    assert!(error
        .to_string()
        .contains("truncated spill batch length prefix"));
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
fn consuming_unique_shared_materialization_moves_memory_batches() {
    let mut buffer = SpillBuffer::unbounded();
    buffer.push(dummy_batch(0, 2)).unwrap();
    buffer.push(dummy_batch(2, 2)).unwrap();
    let shared = buffer.into_shared(vec!["x".into()]).unwrap();

    let mut reader = shared.into_reader().unwrap();
    assert!(matches!(
        &reader.reader,
        SharedSpillReaderSource::OwnedMemory(_)
    ));
    let values = reader
        .by_ref()
        .flat_map(|batch| batch.unwrap().rows)
        .map(|row| match row.get("x") {
            Some(Value::Int(value)) => *value,
            value => panic!("unexpected consumed value: {value:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(values, vec![0, 1, 2, 3]);
}

#[test]
fn consuming_shared_materialization_preserves_other_readers() {
    let mut buffer = SpillBuffer::unbounded();
    buffer.push(dummy_batch(0, 2)).unwrap();
    let shared = buffer.into_shared(vec!["x".into()]).unwrap();
    let retained = shared.clone();

    let reader = shared.into_reader().unwrap();
    assert!(matches!(
        &reader.reader,
        SharedSpillReaderSource::Memory { .. }
    ));
    assert_eq!(reader.count(), 1);
    assert_eq!(retained.reader().unwrap().count(), 1);
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
