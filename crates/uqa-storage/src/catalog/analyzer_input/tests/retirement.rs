//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn catalog_analyzer_input_retires_both_families_on_every_terminal_path() {
    for (steps, failure_key) in [(0, b"b/2".as_slice()), (4, b"l/1".as_slice())] {
        for mode in [
            "error",
            "panic",
            "missing",
            "memory",
            "source_cancel",
            "input_cancel",
            "drop",
        ] {
            let source = source(mode, failure_key);
            let weak = Arc::downgrade(&source);
            let original = source.control.clone();
            let control = StorageReadControl::with_limit(65536);
            let mut input = input(source, &control);
            for _ in 0..steps {
                drop(input.advance().unwrap());
            }
            let occupied = (mode == "memory").then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            if mode == "source_cancel" {
                original.cancellation().cancel();
            }
            if mode == "input_cancel" {
                control.cancellation().cancel();
            }
            if mode != "drop" {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    for _ in 0..4 {
                        input.advance()?;
                    }
                    Ok::<_, StorageBackendError>(())
                }));
                assert!(matches!(result, Err(_) | Ok(Err(_))), "{steps}: {mode}");
                assert!(input.advance().is_err());
                assert!(weak.upgrade().is_none());
            }
            drop(input);
            assert!(weak.upgrade().is_none());
            assert_eq!(
                control.memory().used(),
                occupied
                    .as_ref()
                    .map_or(0, uqa_core::memory::MemoryReservation::bytes)
            );
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn catalog_analyzer_input_rejects_partial_construction_and_encoded_limits_without_leaks() {
    let funded = StorageReadControl::with_limit(65536);
    drop(input(source("none", b""), &funded));
    let peak = funded.memory().peak();
    for limit in (0..peak).step_by(7).chain([peak - 1]) {
        let control = StorageReadControl::with_limit(limit);
        let source = source("none", b"");
        let weak = Arc::downgrade(&source);
        let counts = source.counts.clone();
        assert!(CatalogAnalyzerInput::new(
            source,
            (b"b/", binding),
            (b"l/", label),
            limits(),
            &control
        )
        .is_err());
        assert!(weak.upgrade().is_none());
        assert_eq!(counts.keys.load(Ordering::SeqCst), 0);
        assert_eq!(control.memory().used(), 0);
    }
    for limits in [
        CatalogInputLimits {
            maximum_key_bytes: 0,
            ..limits()
        },
        CatalogInputLimits {
            maximum_value_bytes: 0,
            ..limits()
        },
    ] {
        let control = StorageReadControl::with_limit(65536);
        assert!(CatalogAnalyzerInput::new(
            source("none", b""),
            (b"b/", binding),
            (b"l/", label),
            limits,
            &control
        )
        .is_err());
        assert_eq!(control.memory().used(), 0);
    }
    let source = source("none", b"");
    let weak = Arc::downgrade(&source);
    let control = StorageReadControl::with_limit(65536);
    let mut input = CatalogAnalyzerInput::new(
        source,
        (b"b/", binding),
        (b"l/", label),
        CatalogInputLimits {
            maximum_key_bytes: 2,
            ..limits()
        },
        &control,
    )
    .unwrap();
    assert!(input.advance().is_err());
    assert!(input.advance().is_err());
    assert!(weak.upgrade().is_none());
    assert_eq!(control.memory().used(), 0);
}
