//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::text_load::{CatalogTextLoad, CatalogTextOrder, CatalogTextRows};

fn decode_name(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogTextRow>>> {
    let name = std::str::from_utf8(value).map_err(|_| invalid("invalid name"))?;
    CatalogTextRow::copy(name, std::str::from_utf8(key).unwrap(), control).map(Some)
}
fn load(
    names: &[String],
    order: CatalogTextOrder,
    control: &StorageReadControl,
) -> (CatalogTextLoad, Arc<Counts>) {
    let counts = Arc::<Counts>::default();
    let source = Arc::new(Source {
        rows: names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                (
                    format!("k/{index:03}").into_bytes(),
                    Some(name.as_bytes().to_vec()),
                )
            })
            .collect(),
        counts: counts.clone(),
        control: control.clone(),
        failure: "none",
    });
    let input = CatalogTextInput::new(
        source,
        b"k/",
        decode_name,
        CatalogTextLimits {
            maximum_key_bytes: 128,
            maximum_value_bytes: 1 << 20,
        },
        control,
    )
    .unwrap();
    (CatalogTextLoad::new(input, order, control).unwrap(), counts)
}
fn finish(
    input: &mut CatalogTextLoad,
    steps: usize,
) -> StorageBackendResult<(CatalogTextRows, usize)> {
    for polls in 1..1_000_000 {
        if let Some(rows) = input.advance(steps)? {
            return Ok((rows, polls));
        }
    }
    panic!("catalog text ordering did not progress")
}
#[test]
fn complete_catalog_text_load_preserves_physical_and_stable_name_order_without_copying_values() {
    let names = ["z", "한", "a", "z", "a", "", "z"].map(str::to_owned);
    for order in [CatalogTextOrder::Physical, CatalogTextOrder::Name] {
        let control = StorageReadControl::with_limit(1 << 20);
        let (mut input, _) = load(&names, order, &control);
        let (rows, polls) = finish(&mut input, 1).unwrap();
        assert!(polls > names.len());
        let mut expected = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), format!("k/{i:03}")))
            .collect::<Vec<_>>();
        if matches!(order, CatalogTextOrder::Name) {
            expected.sort_by(|a, b| a.0.cmp(b.0));
        }
        assert_eq!(
            rows.iter()
                .map(|r| (r.name.as_str(), r.value.clone()))
                .collect::<Vec<_>>(),
            expected
        );
        let addresses = rows
            .iter()
            .map(|r| (r.name.as_ptr(), r.value.as_ptr()))
            .collect::<Vec<_>>();
        let (rows, row_memory) = rows.into_parts();
        let moved = rows
            .into_iter()
            .map(crate::catalog::text_load::CatalogTextRecord::into_budgeted)
            .collect::<Vec<_>>();
        assert_eq!(
            moved
                .iter()
                .map(|r| (r.name.as_ptr(), r.value.as_ptr()))
                .collect::<Vec<_>>(),
            addresses
        );
        assert!(input.advance(1).is_err());
        drop((moved, row_memory, input));
        assert_eq!(control.memory().used(), 0);
    }
}
#[test]
fn complete_catalog_text_load_resumes_long_name_comparisons_and_retires_terminal_scratch() {
    let names = (0..24)
        .rev()
        .map(|i| format!("{}_{i:03}", "한prefix".repeat(4096)))
        .collect::<Vec<_>>();
    for steps in [1, 17, 128] {
        let control = StorageReadControl::with_limit(8 << 20);
        let (mut input, _) = load(&names, CatalogTextOrder::Name, &control);
        let (rows, polls) = finish(&mut input, steps).unwrap();
        assert!(polls > 1);
        assert!(rows.windows(2).all(|pair| pair[0].name <= pair[1].name));
        drop((rows, input));
        assert_eq!(control.memory().used(), 0);
        for terminal in ["cancel", "memory", "zero", "drop", "unwind"] {
            let control = StorageReadControl::with_limit(8 << 20);
            let (mut input, counts) = load(&names, CatalogTextOrder::Name, &control);
            assert!(input.advance(steps).unwrap().is_none());
            let physical_complete = counts.decodes.load(Ordering::SeqCst) == names.len();
            let occupied = (terminal == "memory").then(|| {
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap()
            });
            if terminal == "cancel" {
                control.cancellation().cancel();
            }
            if terminal == "unwind" {
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                        let _owner = input;
                        panic!("abandon ordered text");
                    }))
                    .is_err()
                );
            } else {
                if terminal != "drop" {
                    let result = finish(&mut input, usize::from(terminal != "zero"));
                    if terminal == "memory" && physical_complete {
                        let (rows, _) = result.unwrap();
                        assert_eq!(rows.len(), names.len());
                        assert!(rows.windows(2).all(|pair| pair[0].name <= pair[1].name));
                    } else {
                        assert!(result.is_err(), "{terminal}, {steps}");
                    }
                    assert!(input.advance(1).is_err());
                }
                drop(input);
            }
            drop(occupied);
            assert_eq!(control.memory().used(), 0);
        }
    }
}
#[test]
fn complete_catalog_text_load_rejects_foreign_control_and_late_physical_failures() {
    let control = StorageReadControl::with_limit(1 << 20);
    let source = Arc::new(Source {
        rows: [
            (b"k/first".to_vec(), Some(b"z".to_vec())),
            (b"k/last".to_vec(), Some(vec![255])),
        ]
        .into(),
        counts: Arc::default(),
        control: control.clone(),
        failure: "none",
    });
    let input =
        CatalogTextInput::new(source.clone(), b"k/", decode_name, limits(), &control).unwrap();
    assert!(CatalogTextLoad::new(
        input,
        CatalogTextOrder::Name,
        &StorageReadControl::with_limit(1 << 20)
    )
    .is_err());
    let input = CatalogTextInput::new(source, b"k/", decode_name, limits(), &control).unwrap();
    let mut input = CatalogTextLoad::new(input, CatalogTextOrder::Name, &control).unwrap();
    assert_eq!(
        finish(&mut input, 1).err().unwrap().to_string(),
        invalid("invalid name").to_string()
    );
    assert!(input.advance(1).is_err());
    drop(input);
    assert_eq!(control.memory().used(), 0);
}
