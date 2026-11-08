//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn encode(plan: &UnifiedPlan) -> Vec<u8> {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let mut reader = plan.image_reader(&memory, &token).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 29];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(memory.used(), 0);
    bytes
}

fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

#[test]
fn native_bound_plan_images_preserve_queries_and_all_mutation_body_members() {
    for sql in [
        "SELECT (SELECT 1), sum(a) OVER (ORDER BY a) FROM t",
        "WITH q AS (DELETE FROM t RETURNING a) SELECT * FROM q",
        "INSERT INTO t(a, b[1:2]) VALUES (1, ARRAY[2, 3]) ON CONFLICT (a) DO UPDATE SET b[1] = 4 WHERE t.a > 0 RETURNING a",
        "UPDATE t SET (a, b) = (SELECT 1, 2), c[1:2] = ARRAY[3, 4] FROM u WHERE t.a = u.a RETURNING t.a",
        "DELETE FROM t USING u WHERE t.a = u.a RETURNING t.a",
        "MERGE INTO t USING u ON t.a = u.a WHEN MATCHED AND u.a > 3 THEN UPDATE SET b = u.b WHEN MATCHED THEN DELETE WHEN NOT MATCHED BY SOURCE THEN UPDATE SET b = 0 WHEN NOT MATCHED THEN INSERT (a, b) VALUES (u.a, u.b) RETURNING t.a",
    ] {
        let value = plan(sql);
        let bytes = encode(&value);
        let memory = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let restored = UnifiedPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
        assert_eq!(serde_json::to_value(&*restored).unwrap(), serde_json::to_value(&value).unwrap());
        assert_eq!(encode(&restored), bytes);
        assert!(memory.used() > 0);
        std::thread::spawn(move || drop(restored)).join().unwrap();
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_bound_plan_images_reject_partial_invalid_and_unadmitted_ownership() {
    let bytes = encode(&plan("UPDATE t SET a = (SELECT 1) RETURNING a"));
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    for end in 0..bytes.len() {
        assert!(UnifiedPlan::read_image(&mut &bytes[..end], &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(UnifiedPlan::read_image(&mut trailing.as_slice(), &memory, &token).is_err());
    let mut invalid = bytes.clone();
    invalid[16] = 2;
    assert!(UnifiedPlan::read_image(&mut invalid.as_slice(), &memory, &token).is_err());
    invalid = bytes.clone();
    invalid[8] = 2;
    assert!(UnifiedPlan::read_image(&mut invalid.as_slice(), &memory, &token).is_err());
    let denied = MemoryBudget::new(0);
    assert!(UnifiedPlan::read_image(&mut bytes.as_slice(), &denied, &token).is_err());
    assert_eq!(denied.used(), 0);
    token.cancel();
    assert!(UnifiedPlan::read_image(&mut bytes.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_bound_plan_images_retire_deep_owned_roots_without_allocation() {
    let mut bytes = b"UQAPLN01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    bytes.push(0); // Bound query member.
    bytes.push(0); // Original relations_bound.
    bytes.extend(0u64.to_le_bytes()); // No CTEs.
    bytes.push(2); // Native VALUES query.
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(std::iter::repeat_n(14, 10000)); // Unary scalar parents.
    bytes.push(0); // Star leaf.
    bytes.extend(0u64.to_le_bytes()); // Original query arena.
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let value = UnifiedPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
            let remaining = memory.reserve((16 << 20) - memory.used()).unwrap();
            let allocations = allocation_counter::measure(|| drop(value));
            assert_eq!(allocations.count_total, 0);
            drop(remaining);
            assert_eq!(memory.used(), 0);
            for end in [64, bytes.len() - 1] {
                assert!(UnifiedPlan::read_image(&mut &bytes[..end], &memory, &token).is_err());
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
