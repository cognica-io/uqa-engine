//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::MergeTargetColumnBinding;
use std::io::{self, Read};

#[test]
fn native_statement_input_retires_independent_deep_query_images_and_partial_forests() {
    let mut bytes = b"UQASTM01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    bytes.push(0); // SELECT statement.
    for _ in 0..10000 {
        bytes.extend([0; 24]); // Empty window, projection and VALUES vectors.
        bytes.extend([1, 5, 0]); // FROM, subquery source, non-lateral.
    }
    bytes.extend([0; 81]); // An empty original SELECT with eighteen fields.
    for _ in 0..10000 {
        bytes.extend([0; 9]); // Source alias and column aliases.
        bytes.extend([0; 56]); // Remaining original SELECT fields.
    }
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(128 << 20);
            let token = CancellationToken::new();
            let value = Statement::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
            assert_eq!(encode(&value), bytes);
            let remaining = memory.reserve((128 << 20) - memory.used()).unwrap();
            assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
            drop(remaining);
            assert_eq!(memory.used(), 0);
            for end in [31, 128, bytes.len() - 1] {
                assert!(Statement::read_image(&mut &bytes[..end], &memory, &token).is_err());
                assert_eq!(memory.used(), 0);
            }
            let denied = MemoryBudget::new(128 << 10);
            assert!(Statement::read_image(&mut bytes.as_slice(), &denied, &token).is_err());
            assert_eq!(denied.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_statement_input_enforces_exact_peak_admission_and_invalid_handles() {
    // This fixed root has no continuation growth: peak-minus-one must deny its exact payload.
    let value = crate::compile("NOTIFY events, 'retained body'")
        .unwrap()
        .remove(0);
    let bytes = encode(&value);
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    drop(Statement::read_image(&mut bytes.as_slice(), &memory, &token).unwrap());
    let peak = memory.peak();
    for limit in (0..peak).step_by(73).chain([peak - 1]) {
        let denied = MemoryBudget::new(limit);
        assert!(Statement::read_image(&mut bytes.as_slice(), &denied, &token).is_err());
        assert_eq!(denied.used(), 0);
    }
    let exact = MemoryBudget::new(peak);
    drop(Statement::read_image(&mut bytes.as_slice(), &exact, &token).unwrap());
    assert_eq!(exact.used(), 0);
    for invalid in [0, 1, 2] {
        let mut value = crate::compile("SELECT sum(a) OVER () FROM t")
            .unwrap()
            .remove(0);
        let Statement::Select(query) = &mut value else {
            unreachable!()
        };
        match invalid {
            0 => query.windows[0].inherited = Some(0),
            1 => query.windows[0].spec.definition = Some(0),
            _ => {
                let Expr::WindowCall { spec, .. } = &mut query.projections[0].expr else {
                    unreachable!()
                };
                spec.definition = Some(query.windows.len());
            }
        }
        let bytes = encode(&value);
        assert!(Statement::read_image(&mut bytes.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    for index in [0, 8, 16] {
        let mut invalid = bytes.clone();
        invalid[index] = 255;
        assert!(Statement::read_image(&mut invalid.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}

struct Interrupted<'a> {
    bytes: &'a [u8],
    remaining: usize,
    token: &'a CancellationToken,
    mode: u8,
}
impl Read for Interrupted<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            match self.mode {
                0 => return Err(io::Error::other("injected image I/O failure")),
                1 => self.token.cancel(),
                _ => panic!("injected image input unwind"),
            }
        }
        let width = output.len().min(self.remaining).max(1).min(output.len());
        let count = self.bytes.read(&mut output[..width])?;
        self.remaining = self.remaining.saturating_sub(count);
        Ok(count)
    }
}

#[test]
fn native_statement_input_releases_owned_fields_on_io_cancellation_and_unwind() {
    let value = crate::compile("WITH q AS (SELECT (SELECT 1)) SELECT * FROM q")
        .unwrap()
        .remove(0);
    let bytes = encode(&value);
    for mode in 0..3 {
        for remaining in [0, 17, 64, bytes.len() - 1] {
            let memory = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let mut reader = Interrupted {
                bytes: &bytes,
                remaining,
                token: &token,
                mode,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Statement::read_image(&mut reader, &memory, &token)
            }));
            if mode == 2 {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn native_statement_input_preserves_merge_bindings_and_rejects_assignment_addresses() {
    let mut value =
        crate::compile("MERGE INTO t USING u ON t.a = u.a WHEN MATCHED THEN UPDATE SET a = u.a")
            .unwrap()
            .remove(0);
    let Statement::Merge(merge) = &mut value else {
        unreachable!()
    };
    for (name, number) in [("a", 2), ("removed", 4)] {
        merge.target_column_bindings.insert(
            name.into(),
            MergeTargetColumnBinding {
                object_id: [number; 16],
                attribute_number: Some(i16::from(number)),
                domain_dependencies: [45000, 46000].into_iter().collect(),
            },
        );
    }
    check_image(&value);
    let mut value = crate::compile("UPDATE t SET (a,b) = (SELECT 1,2)")
        .unwrap()
        .remove(0);
    let Statement::Update(update) = &mut value else {
        unreachable!()
    };
    let AssignmentTargets::Multiple(targets) = &mut update.assignments[0].0 else {
        unreachable!()
    };
    targets.source_positions[0] = targets.source_width;
    let bytes = encode(&value);
    let memory = MemoryBudget::new(16 << 20);
    assert!(
        Statement::read_image(&mut bytes.as_slice(), &memory, &CancellationToken::new()).is_err()
    );
    assert_eq!(memory.used(), 0);
}
