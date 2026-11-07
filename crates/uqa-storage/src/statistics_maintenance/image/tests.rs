//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn fixture() -> StatisticsMaintenance {
    StatisticsMaintenance {
        object_id: Some([7; 16]),
        generation: 8,
        changes: 9,
        dirty_since_ms: 10,
        analyzed_rows: Some(11),
        statistics_format: 12,
        analyzed_at: Some(13),
        analysis_generation: Some(14),
    }
}

fn encode(value: &StatisticsMaintenance, size: usize) -> Vec<u8> {
    let token = CancellationToken::new();
    let mut reader = value.image_reader(&token).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = vec![0; size];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    token.cancel();
    assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    bytes
}

#[test]
fn native_statistics_maintenance_images_match_independent_inline_bytes() {
    let source = fixture();
    let mut expected = b"UQASMT01".to_vec();
    expected.extend(1u64.to_le_bytes());
    expected.push(1);
    expected.extend([7; 16]);
    for value in [8u64, 9, 10] {
        expected.extend(value.to_le_bytes());
    }
    expected.push(1);
    expected.extend(11u64.to_le_bytes());
    expected.extend(12u32.to_le_bytes());
    for value in [13u64, 14] {
        expected.push(1);
        expected.extend(value.to_le_bytes());
    }
    assert_eq!(expected.len(), 88);
    for size in [1, 17, 4096] {
        assert_eq!(encode(&source, size), expected);
    }
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let restored =
        StatisticsMaintenance::read_image(&mut expected.as_slice(), &memory, &token).unwrap();
    assert_eq!(restored, source);
    assert_eq!(
        restored.next_due_at(false, 100, 12),
        source.next_due_at(false, 100, 12)
    );
    assert_eq!(restored.covers(Some(13)), source.covers(Some(13)));
    let mut bytes = [0; 88];
    let allocation = allocation_counter::measure(|| {
        let mut reader = source.image_reader(&token).unwrap();
        assert_eq!(reader.read(&mut bytes).unwrap(), 88);
        assert_eq!(
            StatisticsMaintenance::read_image(&mut bytes.as_slice(), &memory, &token).unwrap(),
            source
        );
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_statistics_maintenance_images_preserve_absence_and_all_counter_bits() {
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    for mask in 0u8..16 {
        let source = StatisticsMaintenance {
            object_id: (mask & 1 != 0).then_some([0; 16]),
            generation: u64::MAX,
            changes: u64::MAX,
            dirty_since_ms: u64::MAX,
            analyzed_rows: (mask & 2 != 0).then_some(0),
            statistics_format: u32::MAX,
            analyzed_at: (mask & 4 != 0).then_some(u64::MAX),
            analysis_generation: (mask & 8 != 0).then_some(0),
        };
        let bytes = encode(&source, 7);
        assert_eq!(
            StatisticsMaintenance::read_image(&mut bytes.as_slice(), &memory, &token).unwrap(),
            source
        );
    }
    let bytes = encode(&StatisticsMaintenance::default(), 1);
    assert!(bytes[16..].iter().all(|b| *b == 0));
}

#[test]
fn native_statistics_maintenance_input_rejects_malformed_and_truncated_records() {
    let bytes = encode(&fixture(), 7);
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    for length in 0..bytes.len() {
        assert!(StatisticsMaintenance::read_image(&mut &bytes[..length], &memory, &token).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(StatisticsMaintenance::read_image(&mut trailing.as_slice(), &memory, &token).is_err());
    for offset in [0, 8, 16, 57, 70, 79] {
        let mut bad = bytes.clone();
        bad[offset] = 255;
        assert!(StatisticsMaintenance::read_image(&mut bad.as_slice(), &memory, &token).is_err());
    }
    for offset in [16, 57, 70, 79] {
        let mut bad = bytes.clone();
        bad[offset] = 0;
        assert!(StatisticsMaintenance::read_image(&mut bad.as_slice(), &memory, &token).is_err());
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_statistics_maintenance_output_failure_and_cancellation_are_terminal() {
    struct Fault(bool);
    impl Write for Fault {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            assert!(!self.0, "maintenance output unwind");
            Err(std::io::Error::other("maintenance writer"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let source = fixture();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(0);
    for unwind in [false, true] {
        let mut reader = source.image_reader(&token).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reader.write_to(&mut Fault(unwind))
        }));
        assert!(result.is_err() || result.unwrap().is_err());
        assert!(reader.read(&mut []).is_err());
    }
    let mut reader = source.image_reader(&token).unwrap();
    assert_eq!(reader.read(&mut [0; 7]).unwrap(), 7);
    token.cancel();
    assert!(reader.read(&mut [0; 100]).is_err());
    assert!(reader.read(&mut []).is_err());
    assert!(
        StatisticsMaintenance::read_image(&mut encode(&source, 1).as_slice(), &memory, &token)
            .is_err()
    );
}
