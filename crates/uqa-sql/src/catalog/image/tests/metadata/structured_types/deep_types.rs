//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn range_image_validation_walks_deep_type_chains_without_recursion() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for shape in 0..3 {
                let mut value = range();
                for level in 0..10_000 {
                    let inner = std::mem::replace(&mut value.subtype, ColumnType::Void);
                    value.subtype = if shape == 0 || (shape == 2 && level % 2 == 0) {
                        ColumnType::Array(Box::new(inner))
                    } else {
                        ColumnType::Domain {
                            schema: "public".into(),
                            name: "d".into(),
                            oid: 70_000,
                            array_oid: Some(70_001),
                            collation_oid: None,
                            base: Box::new(inner),
                        }
                    };
                }
                let bytes = encode(&singleton("public.r", value));
                let memory = MemoryBudget::new(8 << 20);
                let token = CancellationToken::new();
                let restored = StoredRange::decode(&mut bytes.as_slice(), &memory, &token).unwrap();
                assert_eq!(encode(&restored), bytes);
                drop(restored);
                assert_eq!(memory.used(), 0);
                let memory = MemoryBudget::new(400_000);
                let error = StoredRange::decode(&mut bytes.as_slice(), &memory, &token)
                    .err()
                    .unwrap();
                assert_eq!(error.sqlstate(), Some("53200"));
                assert_eq!(memory.used(), 0);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
