//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_query_input_keeps_source_and_nested_query_namespaces_separate() {
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let mut original =
        query("SELECT * FROM generate_series((SELECT 1), 4) AS g, (SELECT (SELECT 2)) AS q");
    let bytes = encode(&original);
    let restored = QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(
        serde_json::to_value(&*restored).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    drop(restored);
    let RelationalPlan::QueryBlock(block) = &mut original.root else {
        panic!("block")
    };
    assert_eq!(block.subqueries.len(), 1);
    let Some(SourcePlan::Join { right, .. }) = &mut block.from else {
        panic!("join")
    };
    let SourcePlan::Subquery { body, .. } = right.as_mut() else {
        panic!("subquery")
    };
    let RelationalPlan::QueryBlock(child) = &mut body.root else {
        panic!("child")
    };
    child.subqueries.clear(); // Its address zero cannot borrow the outer block's valid arena.
    let bytes = encode(&original);
    assert!(QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_query_input_rejects_dangling_query_and_window_addresses() {
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    for address in [0usize, 1, usize::MAX] {
        let mut scalar = vec![26];
        scalar.extend((address as u64).to_le_bytes());
        let bytes = values_wire(&scalar);
        assert!(QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    let mut original = query("SELECT row_number() OVER (ORDER BY a) FROM t");
    let RelationalPlan::QueryBlock(block) = &mut original.root else {
        panic!("block")
    };
    block.windows.clear();
    let bytes = encode(&original);
    assert!(QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_query_input_rejects_invalid_window_inheritance() {
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    for parent in [0usize, 1, usize::MAX] {
        let mut original = query("SELECT sum(a) OVER w FROM t WINDOW w AS (ORDER BY a)");
        let RelationalPlan::QueryBlock(block) = &mut original.root else {
            panic!("block")
        };
        block.windows[0].inherited = Some(parent);
        let bytes = encode(&original);
        assert!(QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_expression_plan_input_preserves_original_arena_addresses() {
    let mut bytes = b"UQAXPL01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    bytes.push(26); // Native scalar subquery.
    bytes.extend(1u64.to_le_bytes()); // Address the second member, without pruning the first.
    bytes.extend(2u64.to_le_bytes());
    bytes.extend_from_slice(&values_wire(&[0])[16..]);
    bytes.extend_from_slice(&values_wire(&[2])[16..]); // DEFAULT constructor.
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let value = ExpressionPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(value.scalar, ScalarExpr::ScalarSubquery(1));
    assert_eq!(value.subqueries.len(), 2);
    let RelationalPlan::Values { rows, .. } = &value.subqueries[1].root else {
        panic!("values")
    };
    assert_eq!(rows[0][0], ScalarExpr::Default);
    let encoded = collect(value.image_reader(&memory, &token).unwrap());
    assert_eq!(encoded, bytes);
    drop(value);
    assert_eq!(memory.used(), 0);
    bytes[17..25].copy_from_slice(&2u64.to_le_bytes());
    assert!(ExpressionPlan::read_image(&mut bytes.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_query_images_preserve_projection_privilege_slots_and_legacy_framing() {
    use crate::catalog::image::input::window_tests::read_windowed;
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut source = query("SELECT (SELECT 11), (SELECT 22)");
    let legacy = encode(&source);
    let root_tag = 8 + 8 + 1 + 8;
    assert_eq!(legacy[root_tag], 0);
    let mut empty_extension = legacy[..root_tag].to_vec();
    empty_extension.push(3);
    empty_extension.extend(0u64.to_le_bytes());
    empty_extension.extend_from_slice(&legacy[root_tag + 1..]);
    assert!(QueryPlan::read_image(&mut empty_extension.as_slice(), &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
    for slots in [vec![], vec![1], vec![0, 1], vec![1, 0], vec![0, 0], vec![2]] {
        let RelationalPlan::QueryBlock(block) = &mut source.root else {
            panic!("query block")
        };
        block.privilege_subqueries.clone_from(&slots);
        let bytes = encode(&source);
        if slots.is_empty() {
            assert_eq!(bytes, legacy);
        } else {
            let mut expected = legacy[..root_tag].to_vec();
            expected.push(3);
            expected.extend((slots.len() as u64).to_le_bytes());
            for slot in &slots {
                expected.extend((*slot as u64).to_le_bytes());
            }
            expected.extend_from_slice(&legacy[root_tag + 1..]);
            assert_eq!(bytes, expected);
        }
        let restored = QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token);
        if slots == [1, 0] || slots == [0, 0] || slots == [2] {
            assert!(restored.is_err());
        } else {
            let restored = restored.unwrap();
            let resumed = read_windowed(
                QueryPlan::image_input(&memory, &token).unwrap(),
                &bytes,
                1,
                1,
            );
            for value in [&*restored, &*resumed] {
                let RelationalPlan::QueryBlock(block) = &value.root else {
                    panic!("query block")
                };
                assert_eq!(block.privilege_subqueries, slots);
                assert_eq!(encode(value), bytes);
            }
        }
        assert_eq!(memory.used(), 0);
    }
}
