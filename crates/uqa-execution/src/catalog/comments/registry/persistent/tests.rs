//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::comments::{key, restore, CommentAddress};
use uqa_core::memory::ProductionControl;
use uqa_sql::{ast::RelationPersistence, catalog::dependencies::ObjectAddress};

const LIMITS: CatalogInputLimits = CatalogInputLimits {
    maximum_key_bytes: 4096,
    maximum_value_bytes: 1 << 20,
};

fn object(oid: u32) -> CommentAddress {
    ObjectAddress::whole(1247, oid).into()
}
fn fixture(count: u32) -> Arc<dyn CatalogFacade> {
    let storage = uqa_storage_sqlite::SQLiteKeyValueStorage::open_in_memory().unwrap();
    let catalog = Arc::new(storage.catalog());
    for oid in 1..=count {
        catalog
            .set_metadata(
                &key(object(oid)),
                &format!("{oid}:한글\n{}", "x".repeat(16384)),
            )
            .unwrap();
    }
    catalog.set_metadata("unrelated", "invalid").unwrap();
    catalog
}
fn finish(
    input: &mut CommentCatalogInput,
    steps: usize,
) -> StorageBackendResult<(CommentRegistry, usize)> {
    for polls in 1..1_000_000 {
        if let Some(registry) = input.advance(steps)? {
            return Ok((registry, polls));
        }
    }
    panic!("comment input did not progress")
}

#[test]
fn persistent_comment_addresses_preserve_canonical_signed_and_unsigned_boundaries() {
    fn ordinary(text: &str) -> Option<CommentAddress> {
        let mut fields = text.strip_prefix(PREFIX)?.split(':');
        let result = CommentAddress {
            class_id: fields.next()?.parse().ok()?,
            object_id: fields.next()?.parse().ok()?,
            sub_id: fields.next()?.parse().ok()?,
        };
        (fields.next().is_none()
            && result.class_id != 0
            && result.object_id != 0
            && result.sub_id >= 0
            && key(result) == text)
            .then_some(result)
    }
    let numbers = [
        "",
        "0",
        "1",
        "01",
        "+1",
        "-0",
        "-1",
        " 1",
        "1 ",
        "2147483647",
        "2147483648",
        "4294967295",
        "4294967296",
        "999999999999999999999999",
        "١",
        "1\0",
    ];
    for class in numbers {
        for oid in numbers {
            for sub in numbers {
                let text = format!("{PREFIX}{class}:{oid}:{sub}");
                assert_eq!(address(&text), ordinary(&text), "{text:?}");
            }
        }
    }
    for text in ["", "uqa.comment.v1:1:1:1", "uqa.sql.comment.v1:1:1:1:1"] {
        assert_eq!(address(text), ordinary(text));
    }
}

#[test]
fn persistent_comment_input_preserves_native_text_and_future_mutation_ownership() {
    for count in [0, 3, 64] {
        let catalog = fixture(count);
        let before = catalog.metadata_with_prefix("").unwrap();
        let expected = restore(catalog.as_ref(), &CommentRegistry::new()).unwrap();
        let mut visits = Vec::new();
        for steps in [1, 7, 64] {
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(4 << 20);
            let captures = MemoryBudget::new(4 << 20);
            let mut input =
                CommentCatalogInput::new(Arc::clone(&catalog), LIMITS, &mutations, &control)
                    .unwrap();
            let (mut actual, polls) = finish(&mut input, steps).unwrap();
            visits.push(polls);
            assert_eq!(actual, expected);
            assert!(input.advance(1).is_err());
            let captured = actual
                .capture(&ProductionControl::new(
                    &captures,
                    control.cancellation(),
                    control.cancellation(),
                ))
                .unwrap();
            let occupied = control
                .memory()
                .reserve(control.memory().available())
                .unwrap();
            control.cancellation().cancel();
            actual
                .insert(object(100), "later", RelationPersistence::Permanent)
                .unwrap();
            assert_eq!(actual.get(&object(100)).map(String::as_str), Some("later"));
            let token = uqa_core::CancellationToken::new();
            let reader_control = StorageReadControl::with_limit(1 << 20);
            let mut bytes = Vec::new();
            {
                let mut reader = captured.image_reader(&reader_control).unwrap();
                let mut fragment = [0; 257];
                loop {
                    let count = reader.read(&mut fragment).unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&fragment[..count]);
                }
            }
            let original = CommentRegistry::read_image(&mut &bytes[..], &captures, &token).unwrap();
            assert_eq!(original, expected);
            drop((actual, original, captured, input));
            assert_eq!(control.memory().used(), occupied.bytes());
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
            assert_eq!(mutations.used(), 0);
            assert_eq!(captures.used(), 0);
        }
        assert!(visits[0] > visits[1] && visits[1] >= visits[2]);
        assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
    }
}

#[test]
fn persistent_comment_input_retires_physical_rows_and_native_roots_on_every_terminal_path() {
    // Each description needs five text checkpoints; 128 rows keep node admission pending at the last probe.
    let catalog = fixture(128);
    let before = catalog.metadata_with_prefix("").unwrap();
    for cut in [1, 16, 128, 512] {
        for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(16 << 20);
            let mutations = MemoryBudget::new(4 << 20);
            let mut input =
                CommentCatalogInput::new(Arc::clone(&catalog), LIMITS, &mutations, &control)
                    .unwrap();
            for _ in 0..cut {
                assert!(
                    input.advance(1).unwrap().is_none(),
                    "completed before {terminal} at cut {cut}"
                );
            }
            let occupied = (terminal == "memory").then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            match terminal {
                "cancel" => control.cancellation().cancel(),
                "unwind" => {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _owner = input;
                        panic!("retire comments");
                    }));
                    assert_eq!(control.memory().used(), 0);
                    continue;
                }
                _ => (),
            }
            if terminal != "drop" {
                assert!(finish(&mut input, usize::from(terminal != "zero")).is_err());
                assert!(input.advance(1).is_err());
            }
            drop((input, occupied));
            assert_eq!(control.memory().used(), 0);
            assert_eq!(mutations.used(), 0);
        }
    }
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), before);
}

#[test]
fn persistent_comment_input_completes_physical_input_before_address_and_text_validation() {
    for (name, text, expected) in [
        (
            format!("{PREFIX}00:1:0"),
            "invalid\0text",
            "invalid catalog comment address",
        ),
        (key(object(1)), "", "invalid catalog comment text"),
        (key(object(1)), "bad\0text", "invalid catalog comment text"),
    ] {
        let catalog = fixture(0);
        catalog.set_metadata(&name, text).unwrap();
        assert_eq!(
            restore(catalog.as_ref(), &CommentRegistry::new())
                .unwrap_err()
                .to_string(),
            expected
        );
        let control = StorageReadControl::with_limit(4 << 20);
        let failure = |limits| {
            let mut input =
                CommentCatalogInput::new(Arc::clone(&catalog), limits, control.memory(), &control)
                    .unwrap();
            let error = finish(&mut input, 1).unwrap_err();
            assert!(input.advance(1).is_err());
            assert_eq!(control.memory().used(), 0);
            error.to_string()
        };
        assert_eq!(failure(LIMITS), expected);
        let later = key(object(999));
        catalog.set_metadata(&later, &"x".repeat(4096)).unwrap();
        assert_ne!(
            failure(CatalogInputLimits {
                maximum_value_bytes: 2048,
                ..LIMITS
            }),
            expected
        );
        catalog.delete_metadata(&later).unwrap();
        assert_eq!(failure(LIMITS), expected);
    }
}
