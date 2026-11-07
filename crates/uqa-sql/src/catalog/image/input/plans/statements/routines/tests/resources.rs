//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::io::{self, Read};

fn text(bytes: &mut Vec<u8>, text: &str) {
    bytes.extend((text.len() as u64).to_le_bytes());
    bytes.extend(text.as_bytes());
}

fn independent(original: Option<&[u8]>, bound: Option<&[u8]>) -> Vec<u8> {
    let mut bytes = b"UQAROU01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    text(&mut bytes, "public.f");
    bytes.extend(1u64.to_le_bytes());
    bytes.extend([0; 3]); // Legacy optional object identity, revision and OID.
    text(&mut bytes, "public.f");
    bytes.extend([0, 1]); // Replacement flag and procedure kind.
    bytes.extend(0u64.to_le_bytes()); // Parameters.
    bytes.extend([0; 3]); // No RETURNS, result reference or written type.
    text(&mut bytes, "sql");
    if let Some(original) = original {
        bytes.push(1);
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(original);
    } else {
        bytes.push(0);
        text(&mut bytes, "unexamined original body");
    }
    bytes.push(0); // Legacy SQL body form.
    bytes.extend(0u64.to_le_bytes()); // Creation search path.
    bytes.extend([2, 0, 0, 0, 0, 0]); // Volatility, strictness, owner, security and parallel safety.
    bytes.extend([0; 3]); // Support, COST and ROWS.
    bytes.extend(0u64.to_le_bytes()); // Configuration.
    bytes.push(0); // Default EXECUTE ACL.
    if let Some(bound) = bound {
        bytes.push(1);
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(bound);
    } else {
        bytes.push(0);
    }
    bytes
}

#[test]
fn native_routine_images_match_independent_wire_and_reject_invalid_catalog_membership() {
    let bytes = independent(None, None);
    let memory = MemoryBudget::new(1 << 24);
    let token = CancellationToken::new();
    let restored = RoutineRegistry::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    let decoded = &restored["public.f"][0];
    assert!(decoded.def.is_procedure);
    assert_eq!(decoded.def.object_id, None);
    assert!(
        matches!(&decoded.def.body, FunctionBody::Source(value) if value == "unexamined original body")
    );
    assert_eq!(encode(&restored), bytes);
    drop(restored);
    let first = routine(
        "CREATE FUNCTION f() RETURNS integer LANGUAGE SQL RETURN 1",
        1,
    );
    for values in [
        vec![first.clone(), first],
        vec![routine(
            "CREATE FUNCTION f() RETURNS integer LANGUAGE SQL RETURN 1",
            0,
        )],
    ] {
        let source = RoutineRegistry::try_from_iter([("public.f".into(), values)]).unwrap();
        assert!(
            RoutineRegistry::read_image(&mut encode(&source).as_slice(), &memory, &token).is_err()
        );
        assert_eq!(memory.used(), 0);
    }
    let source = RoutineRegistry::try_from_iter([(
        "public.wrong".into(),
        vec![routine(
            "CREATE FUNCTION f() RETURNS integer LANGUAGE SQL RETURN 1",
            1,
        )],
    )])
    .unwrap();
    assert!(RoutineRegistry::read_image(&mut encode(&source).as_slice(), &memory, &token).is_err());
    let mut empty_values = vec![1];
    empty_values.extend(0u64.to_le_bytes());
    assert!(RoutineRegistry::read_image(
        &mut independent(Some(&empty_values), None).as_slice(),
        &memory,
        &token
    )
    .is_err());
    let mut trailing = bytes;
    trailing.push(0);
    assert!(RoutineRegistry::read_image(&mut trailing.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_routine_images_retire_deep_original_and_bound_forests_without_stack_growth() {
    let mut original = vec![1]; // Original VALUES.
    original.extend(1u64.to_le_bytes());
    original.extend(1u64.to_le_bytes());
    original.extend(std::iter::repeat_n(14, 10_000)); // Original unary parents.
    original.push(0);
    let mut bound = vec![0, 0]; // Bound query, original relation binding flag.
    bound.extend(0u64.to_le_bytes()); // CTEs.
    bound.push(2); // Bound VALUES.
    bound.extend(1u64.to_le_bytes());
    bound.extend(1u64.to_le_bytes());
    bound.extend(std::iter::repeat_n(14, 10_000));
    bound.push(0);
    bound.extend(0u64.to_le_bytes());
    let bytes = independent(Some(&original), Some(&bound));
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(128 << 20);
            let token = CancellationToken::new();
            let restored =
                RoutineRegistry::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
            let RoutineBody::Bound(body) = &restored["public.f"][0].body else {
                panic!("bound body")
            };
            let body = body.clone();
            let remaining = memory.reserve(memory.limit() - memory.used()).unwrap();
            assert_eq!(
                allocation_counter::measure(|| drop(restored)).count_total,
                0
            );
            assert_eq!(allocation_counter::measure(|| drop(body)).count_total, 0);
            drop(remaining);
            assert_eq!(memory.used(), 0);
            for end in [128, 4096, bytes.len() - 1] {
                assert!(RoutineRegistry::read_image(&mut &bytes[..end], &memory, &token).is_err());
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
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
                0 => return Err(io::Error::other("injected routine image I/O failure")),
                1 => self.token.cancel(),
                _ => panic!("injected routine image unwind"),
            }
        }
        let width = output.len().min(self.remaining).max(1).min(output.len());
        let count = self.bytes.read(&mut output[..width])?;
        self.remaining = self.remaining.saturating_sub(count);
        Ok(count)
    }
}

#[test]
fn native_routine_images_retire_partial_catalogs_after_io_cancellation_and_unwind() {
    let bytes = encode(&fixture());
    for mode in 0..3 {
        for remaining in [0, 128, bytes.len() / 2, bytes.len() - 1] {
            let memory = MemoryBudget::new(1 << 25);
            let token = CancellationToken::new();
            let mut reader = Interrupted {
                bytes: &bytes,
                remaining,
                token: &token,
                mode,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                RoutineRegistry::read_image(&mut reader, &memory, &token)
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
