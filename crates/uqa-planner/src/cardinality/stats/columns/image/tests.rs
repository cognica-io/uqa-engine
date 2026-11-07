//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn fixture() -> ColumnStatistics {
    [(
        "x".into(),
        ColumnStats {
            distinct_count: 2,
            null_count: 3,
            row_count: 7,
            min_value: Some(Value::Int(-1)),
            max_value: Some(Value::Int(4)),
            histogram: vec![Value::Int(-1), Value::Int(4)],
            mcv_values: vec![Value::Int(4)],
            mcv_frequencies: vec![0.5],
        },
    )]
    .into_iter()
    .collect()
}

fn encode(source: &ColumnStatistics, size: usize) -> Vec<u8> {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut reader = source.image_reader(&memory, &token).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = vec![0; size];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= 16384);
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(memory.used(), 0);
    token.cancel();
    assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    bytes
}

#[test]
fn native_column_statistics_images_match_independent_distribution_bytes() {
    let source = fixture();
    let mut expected = b"UQACST01".to_vec();
    expected.extend(1u64.to_le_bytes());
    expected.extend(1u64.to_le_bytes());
    expected.push(b'x');
    for value in [2u64, 3, 7] {
        expected.extend(value.to_le_bytes());
    }
    expected.extend([1, 1]);
    for value in [2u64, 1, 1] {
        expected.extend(value.to_le_bytes());
    }
    for value in [-1i64, 4, -1, 4, 4] {
        expected.extend(b"UQAVAL01\x03");
        expected.extend(value.to_le_bytes());
    }
    expected.extend(0.5f64.to_bits().to_le_bytes());
    for size in [1, 17, 4096, 20000] {
        assert_eq!(encode(&source, size), expected);
    }
    let memory = MemoryBudget::new(1 << 20);
    let restored =
        ColumnStatistics::read_image(&mut expected.as_slice(), &memory, &CancellationToken::new())
            .unwrap();
    assert_eq!(encode(&restored, 13), expected);
    assert_eq!(restored["x"].equality_selectivity_for(&Value::Int(4)), 0.5);
    assert_eq!(
        restored["x"].equality_selectivity_for(&Value::Int(-1)),
        source["x"].equality_selectivity_for(&Value::Int(-1))
    );
    let reserved = memory.used();
    assert!(reserved > 0);
    let clone = restored.clone();
    drop(restored);
    assert_eq!(memory.used(), reserved);
    drop(clone);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_column_statistics_images_preserve_optional_values_and_frequency_bits() {
    for min in [false, true] {
        for max in [false, true] {
            let stats = ColumnStats {
                distinct_count: u64::MAX,
                null_count: 0,
                row_count: u64::MAX,
                min_value: min.then(|| Value::Str("minimum\0한글".into())),
                max_value: max.then(|| Value::Bytes(vec![0, 255])),
                histogram: vec![
                    Value::List(vec![Value::Int(-7), Value::Null]),
                    Value::Float(-0.0),
                ],
                mcv_values: vec![Value::Null],
                mcv_frequencies: vec![-0.0, f64::INFINITY, f64::from_bits(0x7ff8_0000_0000_1234)],
            };
            let source: ColumnStatistics = [("distribution".into(), stats)].into_iter().collect();
            let bytes = encode(&source, 31);
            let memory = MemoryBudget::new(1 << 20);
            let restored = ColumnStatistics::read_image(
                &mut bytes.as_slice(),
                &memory,
                &CancellationToken::new(),
            )
            .unwrap();
            assert_eq!(encode(&restored, 127), bytes);
            for (left, right) in restored["distribution"]
                .mcv_frequencies
                .iter()
                .zip(&source["distribution"].mcv_frequencies)
            {
                assert_eq!(left.to_bits(), right.to_bits());
            }
            drop(restored);
            assert_eq!(memory.used(), 0);
        }
    }
    let source = ColumnStatistics::default();
    let bytes = encode(&source, 1);
    assert_eq!(bytes, b"UQACST01\0\0\0\0\0\0\0\0");
    let memory = MemoryBudget::new(0);
    assert!(ColumnStatistics::read_image(
        &mut bytes.as_slice(),
        &memory,
        &CancellationToken::new()
    )
    .unwrap()
    .is_empty());
}

#[test]
fn native_column_statistics_input_rejects_invalid_order_and_retires_denied_payloads() {
    let bytes = encode(&fixture(), 4096);
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    for length in 0..bytes.len() {
        assert!(ColumnStatistics::read_image(&mut &bytes[..length], &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(ColumnStatistics::read_image(&mut trailing.as_slice(), &memory, &token).is_err());
    for position in [0, 49, 50] {
        let mut invalid = bytes.clone();
        invalid[position] = 255;
        assert!(ColumnStatistics::read_image(&mut invalid.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    for position in [16, 51, 59, 67] {
        let mut invalid = bytes.clone();
        invalid[position..position + 8].fill(255);
        assert!(ColumnStatistics::read_image(&mut invalid.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    let mut duplicate = bytes.clone();
    duplicate[8..16].copy_from_slice(&2u64.to_le_bytes());
    duplicate.extend(&bytes[16..]);
    assert!(ColumnStatistics::read_image(&mut duplicate.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    let ample = MemoryBudget::new(1 << 20);
    drop(ColumnStatistics::read_image(&mut bytes.as_slice(), &ample, &token).unwrap());
    for limit in [0, 1, ample.peak() - 1] {
        let denied = MemoryBudget::new(limit);
        assert_eq!(
            ColumnStatistics::read_image(&mut bytes.as_slice(), &denied, &token)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(denied.used(), 0);
    }
    let exact = MemoryBudget::new(ample.peak());
    drop(ColumnStatistics::read_image(&mut bytes.as_slice(), &exact, &token).unwrap());
    assert_eq!(exact.used(), 0);
}

#[test]
fn native_column_statistics_output_borrows_payloads_and_retires_active_value_storage() {
    let source: ColumnStatistics = [(
        "x".repeat(1 << 20),
        ColumnStats {
            mcv_values: vec![Value::Str("y".repeat(8 << 20))],
            ..ColumnStats::default()
        },
    )]
    .into_iter()
    .collect();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut buffer = vec![0; 20000];
    let mut written = 0;
    let allocations = allocation_counter::measure(|| {
        let mut reader = source.image_reader(&memory, &token).unwrap();
        assert_eq!(memory.used(), 0);
        loop {
            let count = reader.read(&mut buffer).unwrap();
            assert!(count <= 16384);
            if count == 0 {
                break;
            }
            written += count;
        }
    });
    assert!(written > 9 << 20);
    assert!(allocations.bytes_total < 65536);
    assert_eq!(memory.used(), 0);
    let mut reader = source.image_reader(&memory, &token).unwrap();
    while memory.used() == 0 {
        assert_ne!(reader.read(&mut buffer).unwrap(), 0);
    }
    token.cancel();
    assert!(reader.read(&mut buffer).is_err());
    assert_eq!(memory.used(), 0);
    assert!(reader.read(&mut []).is_err());
    let token = CancellationToken::new();
    let denied = MemoryBudget::new(0);
    let mut reader = source.image_reader(&denied, &token).unwrap();
    loop {
        match reader.read(&mut buffer) {
            Ok(count) => assert_ne!(count, 0),
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"));
                break;
            }
        }
    }
    assert!(reader.read(&mut buffer).is_err());
    assert_eq!(denied.used(), 0);
}

#[test]
fn native_column_statistics_input_retires_io_cancellation_and_unwinding() {
    struct Fault<'a> {
        bytes: &'a [u8],
        offset: usize,
        stop: usize,
        mode: u8,
        token: &'a CancellationToken,
    }
    impl std::io::Read for Fault<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.offset >= self.stop {
                match self.mode {
                    0 => return Err(std::io::Error::other("statistics input")),
                    1 => self.token.cancel(),
                    _ => panic!("statistics unwind"),
                }
            }
            let n = out
                .len()
                .min(self.bytes.len() - self.offset)
                .min(self.stop.saturating_sub(self.offset).max(1));
            out[..n].copy_from_slice(&self.bytes[self.offset..self.offset + n]);
            self.offset += n;
            Ok(n)
        }
    }
    let bytes = encode(&fixture(), 17);
    let memory = MemoryBudget::new(1 << 20);
    for mode in 0..3 {
        for stop in [17, bytes.len() / 2, bytes.len()] {
            let token = CancellationToken::new();
            let mut input = Fault {
                bytes: &bytes,
                offset: 0,
                stop,
                mode,
                token: &token,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ColumnStatistics::read_image(&mut input, &memory, &token)
            }));
            assert!(result.is_err() || result.unwrap().is_err());
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn native_column_statistics_writer_failure_retires_active_traversal() {
    struct Fault(bool);
    impl Write for Fault {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            assert!(!self.0, "statistics output unwind");
            Err(std::io::Error::other("statistics output failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let source: ColumnStatistics = [(
        "x".into(),
        ColumnStats {
            min_value: Some(Value::Str("z".repeat(1 << 20))),
            ..ColumnStats::default()
        },
    )]
    .into_iter()
    .collect();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for unwind in [false, true] {
        let mut reader = source.image_reader(&memory, &token).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reader.write_to(&mut Fault(unwind))
        }));
        assert!(result.is_err() || result.unwrap().is_err());
        assert_eq!(memory.used(), 0);
        assert!(reader.read(&mut []).is_err());
    }
}
