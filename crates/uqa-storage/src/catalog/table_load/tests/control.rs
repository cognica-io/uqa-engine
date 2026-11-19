//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn catalog_table_load_rejects_a_different_retained_context_before_source_reads() {
    for mismatch in ["memory", "cancellation", "files"] {
        let original = StorageReadControl::with_limit(1 << 20);
        let other = match mismatch {
            "memory" => StorageReadControl::with_limit(1 << 20),
            "cancellation" => {
                StorageReadControl::new(original.memory(), &uqa_core::CancellationToken::new())
            }
            _ => original.with_temporary_limit(1 << 20),
        };
        let source = Arc::new(source());
        let input = CatalogTableInput::new(
            source.clone(),
            (b"d/", definition),
            (b"a/", replacement),
            CatalogInputLimits {
                maximum_key_bytes: 256,
                maximum_value_bytes: 1 << 20,
            },
            &original,
        )
        .unwrap();
        assert!(original.memory().used() > 0);
        let result =
            CatalogTableLoad::new(input, CatalogTableOrder::BeforeReplacements, None, &other);
        assert!(result.is_err(), "{mismatch}");
        drop(result);
        assert_eq!(source.reads.load(Ordering::SeqCst), 0);
        assert_eq!(original.memory().used(), 0);
        assert_eq!(other.memory().used(), 0);
        if mismatch == "memory" {
            assert_eq!(other.memory().peak(), 0);
        }
    }
}

#[test]
fn catalog_table_load_accepts_clones_of_its_original_retained_context() {
    let original = StorageReadControl::with_limit(1 << 20).with_temporary_limit(1 << 20);
    let source = Arc::new(source());
    let expected = expected(&source);
    let input = CatalogTableInput::new(
        source,
        (b"d/", definition),
        (b"a/", replacement),
        CatalogInputLimits {
            maximum_key_bytes: 256,
            maximum_value_bytes: 1 << 20,
        },
        &original,
    )
    .unwrap();
    let mut load = CatalogTableLoad::new(
        input,
        CatalogTableOrder::AfterReplacements,
        None,
        &original.clone(),
    )
    .unwrap();
    let tables = finish(&mut load, 1).unwrap();
    assert_eq!(tables.len(), expected.len());
    for (table, expected) in tables.iter().zip(expected) {
        assert_eq!(
            serde_json::to_value(&**table).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
    drop((tables, load));
    assert_eq!(original.memory().used(), 0);
}
