//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{self, Read};

fn deep_wire(depth: usize, scalar_depth: usize) -> Vec<u8> {
    let mut bytes = b"UQAQRY01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    for level in 0..=depth {
        bytes.push(0);
        bytes.extend(0u64.to_le_bytes());
        bytes.push(2);
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        if level == depth {
            bytes.extend(std::iter::repeat_n(14, scalar_depth));
        } // UnaryMinus chain.
        bytes.push(0);
        bytes.extend(u64::from(level < depth).to_le_bytes());
    }
    bytes
}

#[test]
fn native_query_input_retires_deep_full_and_partial_forests_without_stack_or_allocation_growth() {
    let bytes = deep_wire(10_000, 10_000);
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(128 << 20);
            let token = CancellationToken::new();
            let value = QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
            let mut node = &*value;
            let mut depth = 0;
            loop {
                let RelationalPlan::Values { subqueries, .. } = &node.root else {
                    panic!("values")
                };
                if subqueries.is_empty() {
                    break;
                }
                node = &subqueries[0];
                depth += 1;
            }
            assert_eq!(depth, 10_000);
            let allocations = allocation_counter::measure(|| drop(value));
            assert_eq!(allocations.count_total, 0);
            assert_eq!(memory.used(), 0);
            for length in [bytes.len() - 1, bytes.len() - 100, bytes.len() / 2] {
                assert!(QueryPlan::read_image(&mut &bytes[..length], &memory, &token).is_err());
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_query_input_admits_exact_peak_and_releases_exhausted_owners() {
    // A single native node uses no growing continuation buffer, so its peak is the required admission rather than a preferred growth capacity.
    let bytes = encode(&query("VALUES (7)"));
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    let value = QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    let peak = memory.peak();
    assert!(memory.used() > 0);
    drop(value);
    assert_eq!(memory.used(), 0);
    for limit in [0, 1, peak / 4, peak / 2, peak - 1] {
        let memory = MemoryBudget::new(limit);
        assert!(
            QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).is_err(),
            "limit {limit}"
        );
        assert_eq!(memory.used(), 0, "limit {limit}");
    }
    let memory = MemoryBudget::new(peak);
    let value = QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    let remaining = memory.reserve(peak - memory.used()).unwrap();
    let allocations = allocation_counter::measure(|| drop(value));
    assert_eq!(allocations.count_total, 0);
    drop(remaining);
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_query_input_keeps_admission_when_continuation_growth_fits_a_smaller_budget() {
    let bytes = encode(&query("WITH q AS (INSERT INTO t(a) SELECT (SELECT 7) RETURNING a) SELECT * FROM q, (VALUES (1)) v"));
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(16 << 20);
    drop(QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap());
    let peak = memory.peak();
    let mut denied = 0;
    let mut accepted = 0;
    for limit in (0..peak).step_by(73).chain([peak - 1, peak]) {
        let memory = MemoryBudget::new(limit);
        match QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token) {
            Ok(value) => {
                accepted += 1;
                assert_eq!(encode(&value), bytes);
                assert!(memory.used() <= limit);
                let remaining = memory.reserve(limit - memory.used()).unwrap();
                assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
                drop(remaining);
            }
            Err(_) => denied += 1,
        }
        assert!(memory.peak() <= limit);
        assert_eq!(memory.used(), 0);
    }
    assert!(denied > 0 && accepted > 0);
}

struct Interrupted<'a> {
    input: &'a [u8],
    remaining: usize,
    panic: bool,
    cancel: Option<&'a CancellationToken>,
}

impl Read for Interrupted<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            assert!(!self.panic, "injected image-reader unwind");
            if let Some(token) = self.cancel.take() {
                token.cancel();
                self.remaining = self.input.len();
            } else {
                return Err(io::Error::other("injected image-reader failure"));
            }
        }
        let count = self
            .remaining
            .min(buffer.len())
            .min(self.input.len())
            .min(7);
        buffer[..count].copy_from_slice(&self.input[..count]);
        self.input = &self.input[count..];
        self.remaining -= count;
        Ok(count)
    }
}

#[test]
fn native_query_input_releases_partial_owners_on_io_cancel_and_unwind() {
    let bytes = deep_wire(100, 1000);
    for offset in [16, 100, bytes.len() - 1] {
        for mode in 0..3 {
            let token = CancellationToken::new();
            let memory = MemoryBudget::new(8 << 20);
            let mut reader = Interrupted {
                input: &bytes,
                remaining: offset,
                panic: mode == 2,
                cancel: (mode == 1).then_some(&token),
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                QueryPlan::read_image(&mut reader, &memory, &token)
            }));
            if mode == 2 {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(memory.used(), 0, "offset {offset}, mode {mode}");
        }
    }
}

#[test]
fn native_query_input_owns_cross_thread_payloads_after_source_retirement() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<CapturedQuery>();
    send_sync::<CapturedExpressionPlan>();
    let original = query("SELECT (SELECT 9) FROM (VALUES (1), (2)) AS q(a)");
    let bytes = encode(&original);
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let value = QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    drop(original);
    drop(bytes);
    let result = std::thread::spawn(move || {
        let RelationalPlan::QueryBlock(block) = &value.root else {
            panic!("block")
        };
        assert_eq!(block.subqueries.len(), 1);
        drop(value);
    });
    result.join().unwrap();
    assert_eq!(memory.used(), 0);
}
