//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::Read;

#[test]
fn native_table_metadata_input_retires_partial_invalid_and_unfunded_images() {
    let token = CancellationToken::new();
    for component in complete_components() {
        let bytes = component.encode(127);
        let memory = MemoryBudget::new(1 << 20);
        for length in 0..bytes.len() {
            assert!(component
                .decode(&mut &bytes[..length], &memory, &token)
                .is_err());
            assert_eq!(memory.used(), 0, "prefix {length}");
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(component
            .decode(&mut trailing.as_slice(), &memory, &token)
            .is_err());
        let mut count = bytes.clone();
        count[8..16].fill(255);
        assert!(component
            .decode(&mut count.as_slice(), &memory, &token)
            .is_err());
        let mut tag = bytes.clone();
        tag[match component {
            Component::Hierarchy(_) => 32,
            Component::Dropped(_) => 42,
            _ => 16,
        }] = 255;
        assert!(component
            .decode(&mut tag.as_slice(), &memory, &token)
            .is_err());
        assert_eq!(memory.used(), 0);
        let ample = MemoryBudget::new(1 << 20);
        drop(
            component
                .decode(&mut bytes.as_slice(), &ample, &token)
                .unwrap(),
        );
        let peak = ample.peak();
        for limit in [0, 1, peak - 1] {
            let denied = MemoryBudget::new(limit);
            assert_eq!(
                component
                    .decode(&mut bytes.as_slice(), &denied, &token)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
            assert_eq!(denied.used(), 0);
        }
        let exact = MemoryBudget::new(peak);
        drop(
            component
                .decode(&mut bytes.as_slice(), &exact, &token)
                .unwrap(),
        );
        assert_eq!(exact.used(), 0);
    }
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
                0 => {
                    return Err(std::io::Error::other(
                        "injected table metadata read failure",
                    ))
                }
                1 => self.token.cancel(),
                _ => panic!("injected table metadata unwind"),
            }
        }
        let count = output
            .len()
            .min(self.bytes.len() - self.offset)
            .min(self.stop.saturating_sub(self.offset).max(1));
        output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

#[test]
fn native_table_metadata_input_retires_io_cancellation_and_unwinding() {
    for component in complete_components() {
        let bytes = component.encode(127);
        let memory = MemoryBudget::new(1 << 20);
        for failure in 0..3 {
            for stop in [17, bytes.len() / 2, bytes.len()] {
                let token = CancellationToken::new();
                let mut source = Interrupted {
                    bytes: &bytes,
                    offset: 0,
                    stop,
                    failure,
                    token: &token,
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    component.decode(&mut source, &memory, &token)
                }));
                assert!(result.is_err() || result.unwrap().is_err());
                assert_eq!(memory.used(), 0);
            }
        }
    }
}

#[test]
fn native_table_metadata_output_is_bounded_by_active_traversal() {
    let token = CancellationToken::new();
    let mut values = complete_components();
    let Component::Hierarchy(hierarchy) = &mut values[3] else {
        unreachable!()
    };
    hierarchy.parents[0] = "original".repeat(1 << 20);
    for component in values {
        let memory = MemoryBudget::new(1 << 20);
        let mut buffer = vec![0; 20000];
        let measured = allocation_counter::measure(|| {
            let mut reader = component.reader(&memory, &token).unwrap();
            assert_eq!(memory.used(), 0);
            loop {
                let count = reader.read(&mut buffer).unwrap();
                assert!(count <= 16384);
                if count == 0 {
                    break;
                }
            }
        });
        assert!(measured.bytes_total < 16384);
        assert_eq!(measured.bytes_current, 0);
        assert_eq!(memory.used(), 0);
        let zero = MemoryBudget::new(0);
        let mut reader = component.reader(&zero, &token).unwrap();
        assert_eq!(
            reader.read(&mut buffer).unwrap_err().sqlstate(),
            Some("53200")
        );
        assert!(reader.read(&mut []).is_err());
        assert_eq!(zero.used(), 0);
        let token = CancellationToken::new();
        let mut reader = component.reader(&memory, &token).unwrap();
        reader.read(&mut [0; 32]).unwrap();
        token.cancel();
        assert!(reader.read(&mut buffer).is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_table_metadata_input_owns_deep_partition_scalars() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let mut expression = Expr::Star;
            for _ in 0..10000 {
                expression = Expr::Not(Box::new(expression));
            }
            let component = Component::Hierarchy(TableHierarchy {
                partition_bound: Some(PartitionBound::List(vec![expression])),
                ..TableHierarchy::default()
            });
            let bytes = component.encode(127);
            let memory = MemoryBudget::new(64 << 20);
            let token = CancellationToken::new();
            let saved = component
                .decode(&mut bytes.as_slice(), &memory, &token)
                .unwrap();
            assert_eq!(saved.encode(31), bytes);
            drop(saved);
            assert!(component
                .decode(&mut &bytes[..bytes.len() - 1], &memory, &token)
                .is_err());
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
