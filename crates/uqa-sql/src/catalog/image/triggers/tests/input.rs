//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Read;

#[test]
fn native_trigger_input_rejects_partial_malformed_and_mismatched_images() {
    let wire = wire::fixture();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    for length in 0..wire.bytes.len() {
        assert!(
            TriggerCatalog::read_image(&mut &wire.bytes[..length], &memory, &token).is_err(),
            "prefix {length}"
        );
        assert_eq!(memory.used(), 0, "prefix {length}");
    }
    let mut cases = Vec::new();
    for field in [
        "function_flag",
        "enabled",
        "constraint",
        "deferrability",
        "row",
        "timing",
        "replace",
        "when",
        "event",
        "transition_new",
        "transition_table",
    ] {
        let mut bytes = wire.bytes.clone();
        bytes[wire.positions[field]] = 255;
        cases.push(bytes);
    }
    for field in ["function_id", "object_id"] {
        let mut bytes = wire.bytes.clone();
        let start = wire.positions[field];
        bytes[start..start + 16].fill(0);
        cases.push(bytes);
    }
    for field in ["oid", "constraint_oid"] {
        for oid in [0i64, -1, i64::from(u32::MAX) + 1] {
            let mut bytes = wire.bytes.clone();
            let start = wire.positions[field];
            bytes[start..start + 8].copy_from_slice(&oid.to_le_bytes());
            cases.push(bytes);
        }
    }
    for start in [
        0,
        24,
        wire.positions["name"] + 8,
        wire.positions["table"] + 8,
        wire.positions["condition"],
    ] {
        let mut bytes = wire.bytes.clone();
        bytes[start] = 0;
        cases.push(bytes);
    }
    let mut condition = wire.bytes.clone();
    condition[wire.positions["condition"] + 8] = 2;
    cases.push(condition);
    let mut extra = wire.bytes.clone();
    extra.push(0);
    cases.push(extra);
    for bytes in cases {
        assert_eq!(
            TriggerCatalog::read_image(&mut bytes.as_slice(), &memory, &token)
                .err()
                .unwrap()
                .sqlstate(),
            Some("XX001")
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_trigger_input_rejects_duplicate_keys_incarnations_and_catalog_oids() {
    let wire = wire::fixture();
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let mut cases = Vec::new();
    let mut relations = wire.bytes.clone();
    relations[8] = 2;
    relations.extend(&wire.bytes[16..]);
    cases.push(relations);
    for distinct in 0..4 {
        let start = wire.positions["entry"];
        let mut second = wire.bytes[start..].to_vec();
        if distinct >= 1 {
            second[8] = b'x';
            second[wire.positions["name"] - start + 8] = b'x';
        }
        if distinct >= 2 {
            let offset = wire.positions["object_id"] - start;
            second[offset..offset + 16].fill(3);
        }
        if distinct >= 3 {
            let offset = wire.positions["oid"] - start;
            second[offset..offset + 8].copy_from_slice(&65538i64.to_le_bytes());
        }
        let mut bytes = wire.bytes.clone();
        bytes[wire.positions["entry_count"]] = 2;
        bytes.extend(second);
        cases.push(bytes);
    }
    let mut temporary = wire.bytes.clone();
    let replacement = [9u64.to_le_bytes().as_slice(), b"pg_temp_1"].concat();
    temporary.splice(16..25, replacement);
    cases.push(temporary);
    for bytes in cases {
        assert_eq!(
            TriggerCatalog::read_image(&mut bytes.as_slice(), &memory, &token)
                .err()
                .unwrap()
                .sqlstate(),
            Some("XX001")
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_trigger_input_admits_exact_peak_and_retires_every_interruption() {
    let bytes = wire::fixture().bytes;
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    drop(TriggerCatalog::read_image(&mut bytes.as_slice(), &memory, &token).unwrap());
    let peak = memory.peak();
    assert_eq!(memory.used(), 0);
    for limit in [0, 1, peak - 1] {
        let memory = MemoryBudget::new(limit);
        assert_eq!(
            TriggerCatalog::read_image(&mut bytes.as_slice(), &memory, &token)
                .err()
                .unwrap()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(memory.used(), 0);
    }
    let exact = MemoryBudget::new(peak);
    drop(TriggerCatalog::read_image(&mut bytes.as_slice(), &exact, &token).unwrap());
    assert_eq!(exact.used(), 0);
    for failure in 0..3 {
        for stop in [32, 128, bytes.len() - 1, bytes.len()] {
            let token = CancellationToken::new();
            let mut reader = Interrupted {
                bytes: &bytes,
                offset: 0,
                stop,
                failure,
                token: &token,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                TriggerCatalog::read_image(&mut reader, &memory, &token)
            }));
            assert!(result.is_err() || result.unwrap().is_err());
            assert_eq!(memory.used(), 0);
        }
    }
    let mut enormous = wire::fixture();
    let offset = enormous.positions["arguments"];
    enormous.bytes[offset..offset + 8].fill(255);
    assert!(TriggerCatalog::read_image(&mut enormous.bytes.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

struct Interrupted<'a> {
    bytes: &'a [u8],
    offset: usize,
    stop: usize,
    failure: u8,
    token: &'a CancellationToken,
}
impl Read for Interrupted<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.offset >= self.stop {
            match self.failure {
                0 => return Err(std::io::Error::other("injected input failure")),
                1 => self.token.cancel(),
                _ => panic!("injected input unwind"),
            }
        }
        let mut count = output.len().min(self.bytes.len() - self.offset);
        if self.offset < self.stop {
            count = count.min(self.stop - self.offset);
        }
        output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}
