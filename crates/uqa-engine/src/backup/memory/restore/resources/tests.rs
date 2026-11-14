//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::ProductionControl;

struct Unread;

impl Read for Unread {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("identity admission must not read the source");
    }
}

fn address(kind: u8, key: &str, control: &StorageReadControl) -> DecodedMemoryImageAddress {
    let value = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text(key)
    .unwrap()
    .into_budgeted()
    .unwrap();
    let name = MemoryImageName::from_budgeted(value).unwrap();
    if kind == 0 {
        DecodedMemoryImageAddress::FieldAnalyzer {
            table: name.clone(),
            field: name,
        }
    } else {
        DecodedMemoryImageAddress::Resource { kind, name }
    }
}

#[test]
fn resource_input_owns_selection_admission_and_incomplete_retirement() {
    for kind in 0..=4 {
        for mode in 0..5 {
            let control = StorageReadControl::with_limit(1 << 20);
            let mutations = MemoryBudget::new(1 << 20);
            let resources = AnalyzerResources::default();
            let key = "selected\0한글".repeat(1025);
            let selected = address(kind, &key, &control);
            let changed = address(kind, if mode == 4 { &key } else { "another" }, &control);
            let retained = control.memory().used();
            let (input, complete) = ResourcesInput::new(control.memory())
                .unwrap()
                .read(
                    &selected,
                    &mut Unread,
                    &resources,
                    &mutations,
                    ImageDecodeWindow { bytes: 1, steps: 1 },
                    &control,
                )
                .unwrap();
            assert!(!complete);
            assert!(input.fields().is_err());
            assert!(control.memory().used() > retained);
            if mode == 0 || mode == 4 {
                assert!(input
                    .read(
                        &changed,
                        &mut Unread,
                        &resources,
                        &mutations,
                        ImageDecodeWindow { bytes: 1, steps: 1 },
                        &control
                    )
                    .is_err());
            } else if mode == 1 {
                assert!(input
                    .read(
                        &selected,
                        &mut Unread,
                        &resources,
                        &mutations,
                        ImageDecodeWindow { bytes: 0, steps: 1 },
                        &control
                    )
                    .is_err());
            } else if mode == 2 {
                control.cancellation().cancel();
                assert!(input
                    .read(
                        &selected,
                        &mut Unread,
                        &resources,
                        &mutations,
                        ImageDecodeWindow { bytes: 1, steps: 1 },
                        &control
                    )
                    .is_err());
            } else {
                drop(input);
            }
            assert_eq!(control.memory().used(), retained);
            drop((selected, changed));
            assert_eq!(control.memory().used(), 0);
            assert_eq!(mutations.used(), 0);
        }
    }
}
