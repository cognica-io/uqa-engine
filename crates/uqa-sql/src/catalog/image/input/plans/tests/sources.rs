//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_query_input_keeps_source_bindings_and_internal_value_descriptors() {
    let mut value = query("SELECT * FROM (VALUES (1)) v(a), generate_series(1, 2) g");
    let RelationalPlan::QueryBlock(block) = &mut value.root else {
        panic!("block")
    };
    let Some(SourcePlan::Join { left, right, .. }) = &mut block.from else {
        panic!("join")
    };
    let SourcePlan::Values {
        internal_relation,
        internal_column_types,
        internal_column_collations,
        ..
    } = left.as_mut()
    else {
        panic!("values")
    };
    *internal_relation = Some(crate::ast::InternalRelationId::from_raw(917));
    *internal_column_types = vec![
        Some(crate::ColumnType::Array(Box::new(
            crate::ColumnType::Integer,
        ))),
        None,
    ];
    *internal_column_collations = vec![Some(950), None];
    let SourcePlan::Function {
        binding, relations, ..
    } = right.as_mut()
    else {
        panic!("function")
    };
    *binding = Some(crate::ast::FunctionBinding {
        object_id: Some([71; 16]),
        name: "selected.function".into(),
        argument_types: vec!["int4".into()],
        builtin: false,
        dispatch: None,
        invocation: None,
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    });
    *relations = Some(crate::ast::OperatorJoinRelations {
        left: "original.left".into(),
        right: "original.right".into(),
    });
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let bytes = encode(&value);
    let restored = QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
    assert_eq!(
        serde_json::to_value(&*restored).unwrap(),
        serde_json::to_value(&value).unwrap()
    );
    assert_eq!(encode(&restored), bytes);
    drop(restored);
    assert_eq!(memory.used(), 0);
}

fn table_wire() -> Vec<u8> {
    let mut bytes = vec![0]; // Table constructor.
    for _ in 0..2 {
        bytes.extend(1u64.to_le_bytes());
        bytes.push(b't');
    }
    bytes.push(0); // No alias.
    bytes.extend(0u64.to_le_bytes()); // Empty column aliases.
    bytes.extend([0, 1]); // Unbound columns, include descendants.
    bytes
}

fn join_wire(depth: usize) -> (Vec<u8>, usize) {
    let mut bytes = b"UQAQRY01".to_vec();
    bytes.extend(1u64.to_le_bytes());
    bytes.push(0);
    bytes.extend(0u64.to_le_bytes());
    bytes.push(0); // Query block.
    for _ in 0..3 {
        bytes.extend(0u64.to_le_bytes());
    } // Windows, projections, privilege columns.
    bytes.push(1); // Source is present.
    for _ in 0..depth {
        bytes.extend([1, 0]);
    } // Join constructor and comma flag before its left child.
    let partial = bytes.len();
    bytes.extend(table_wire());
    for _ in 0..depth {
        bytes.extend(table_wire()); // Right child.
        bytes.extend([0, 0, 0, 0, 0]); // Inner, ON absent, USING absent, not natural, alias absent.
        bytes.extend(0u64.to_le_bytes()); // Column aliases.
        bytes.extend([0, 0]); // Lateral and automatic join strategy.
    }
    bytes.extend([0, 0]); // WHERE absent and projection computation.
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes()); // Grouping vectors.
    bytes.extend([0, 0]); // Group distinct and HAVING.
    bytes.extend(0u64.to_le_bytes()); // Ordering.
    bytes.extend([0, 0, 0, 0]); // LIMIT, WITH TIES, OFFSET, DISTINCT.
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes()); // DISTINCT ON and query arena.
    bytes.push(0);
    bytes.extend(0u64.to_le_bytes()); // Row access and locking.
    (bytes, partial)
}

#[test]
fn native_query_input_retires_deep_required_source_placeholders() {
    let (bytes, partial) = join_wire(10_000);
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(128 << 20);
            let token = CancellationToken::new();
            let value = QueryPlan::read_image(&mut bytes.as_slice(), &memory, &token).unwrap();
            let RelationalPlan::QueryBlock(block) = &value.root else {
                panic!("block")
            };
            let mut source = block.from.as_ref().unwrap();
            let mut depth = 0;
            while let SourcePlan::Join { left, .. } = source {
                depth += 1;
                source = left;
            }
            assert_eq!(depth, 10_000);
            let allocations = allocation_counter::measure(|| drop(value));
            assert_eq!(allocations.count_total, 0);
            assert_eq!(memory.used(), 0);
            for length in [partial - 1, partial, partial + 1, bytes.len() - 1] {
                assert!(QueryPlan::read_image(&mut &bytes[..length], &memory, &token).is_err());
                assert_eq!(memory.used(), 0, "prefix {length}");
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
