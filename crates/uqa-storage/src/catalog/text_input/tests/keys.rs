//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::input::{CatalogInput, CatalogInputStep};

fn key(key: &[u8], control: &StorageReadControl) -> StorageBackendResult<Option<Budgeted<String>>> {
    let key = std::str::from_utf8(key).map_err(|_| invalid("invalid selected key"))?;
    let mut value = BudgetedString::new(control.memory());
    value.push_str(key)?;
    let (value, memory) = value.into_parts();
    Ok(Some(Budgeted::new(value, memory)))
}

#[test]
fn catalog_key_input_ignores_values_and_counts_tombstones_as_original_slots() {
    let counts = Arc::<Counts>::default();
    let source = Arc::new(Source {
        rows: BTreeMap::from([
            (b"k/absent".to_vec(), None),
            (b"k/large".to_vec(), Some(vec![255; 1 << 20])),
            (b"k/selected".to_vec(), Some(vec![255])),
        ]),
        counts: counts.clone(),
        control: StorageReadControl::with_limit(65536),
        failure: "panic",
    });
    let control = StorageReadControl::with_limit(4096);
    let mut input = CatalogInput::new_keys(
        source.clone(),
        b"k/",
        key,
        CatalogTextLimits {
            maximum_value_bytes: 1,
            ..limits()
        },
        &control,
    )
    .unwrap();
    assert!(matches!(
        input.advance().unwrap(),
        CatalogInputStep::Pending
    ));
    assert_eq!(counts.keys.load(Ordering::SeqCst), 1);
    for name in ["k/large", "k/selected"] {
        let CatalogInputStep::Row(value) = input.advance().unwrap() else {
            panic!("selected name")
        };
        assert_eq!(&**value, name);
    }
    assert!(matches!(
        input.advance().unwrap(),
        CatalogInputStep::Complete
    ));
    assert!(matches!(
        input.advance().unwrap(),
        CatalogInputStep::Complete
    ));
    assert_eq!(counts.keys.load(Ordering::SeqCst), 3);
    assert_eq!(counts.values.load(Ordering::SeqCst), 0);
    assert_eq!(counts.decodes.load(Ordering::SeqCst), 0);
    assert_eq!(Arc::strong_count(&source), 1);
    drop(input);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_key_input_retires_original_controls_invalid_keys_and_denied_admission() {
    for terminal in [
        "source_cancel",
        "cancel",
        "memory",
        "key",
        "encoded",
        "drop",
    ] {
        let mut source = source("panic");
        if terminal == "key" {
            Arc::get_mut(&mut source).unwrap().rows =
                BTreeMap::from([(vec![b'k', b'/', 255], Some(vec![]))]);
        }
        let control = StorageReadControl::with_limit(4096);
        let selected_limits = if terminal == "encoded" {
            CatalogTextLimits {
                maximum_key_bytes: 3,
                ..limits()
            }
        } else {
            limits()
        };
        let mut input =
            CatalogInput::new_keys(source.clone(), b"k/", key, selected_limits, &control).unwrap();
        let occupied = if terminal == "memory" {
            Some(
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap(),
            )
        } else {
            None
        };
        if terminal == "cancel" {
            control.cancellation().cancel();
        }
        if terminal == "source_cancel" {
            source.control.cancellation().cancel();
        }
        if terminal != "drop" {
            assert!(input.advance().is_err(), "{terminal}");
            assert!(input.advance().is_err());
        }
        drop((input, occupied));
        assert_eq!(Arc::strong_count(&source), 1);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(source.counts.values.load(Ordering::SeqCst), 0);
    }
}
