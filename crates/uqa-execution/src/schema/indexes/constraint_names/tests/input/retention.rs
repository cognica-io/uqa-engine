//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn admitted_key_name_maps_retain_exact_nodes_and_release_every_failed_admission() {
    let row = row();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        let mut input = KeyConstraintNameInput::new(Some("2"), &memory, &token, &token).unwrap();
        input.push(&row).unwrap();
        output = Some(input.finish().unwrap());
    });
    assert_eq!(
        usize::try_from(allocation.bytes_current).unwrap(),
        memory.used()
    );
    assert!(usize::try_from(allocation.bytes_max).unwrap() <= memory.peak());
    let peak = memory.peak();
    drop(output);
    assert_eq!(memory.used(), 0);
    for limit in (0..peak).step_by(37).chain([peak - 1]) {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        let mut input = KeyConstraintNameInput::new(Some("2"), &memory, &token, &token).unwrap();
        assert!(input.push(&row).is_err(), "{limit}/{peak}");
        assert!(input.finish().is_err());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn admitted_key_name_input_checks_both_owners_at_entry_progress_and_completion() {
    for original_cancelled in [false, true] {
        for boundary in ["entry", "progress", "completion", "drop"] {
            let memory = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let cancel = if original_cancelled {
                &original
            } else {
                &invoking
            };
            if boundary == "entry" {
                cancel.cancel();
                assert!(matches!(
                    KeyConstraintNameInput::new(Some("2"), &memory, &original, &invoking),
                    Err(StorageBackendError::Cancelled(_))
                ));
                assert_eq!(memory.peak(), 0);
                continue;
            }
            let mut input =
                KeyConstraintNameInput::new(Some("2"), &memory, &original, &invoking).unwrap();
            input.push(&row()).unwrap();
            assert!(memory.used() > 0);
            cancel.cancel();
            if boundary == "progress" {
                assert!(matches!(
                    input.push(&row()),
                    Err(StorageBackendError::Cancelled(_))
                ));
                assert!(input.finish().is_err());
            } else if boundary == "completion" {
                assert!(matches!(
                    input.finish(),
                    Err(StorageBackendError::Cancelled(_))
                ));
            } else {
                drop(input);
            }
            assert_eq!(memory.used(), 0);
        }
    }
}

#[test]
fn admitted_key_name_restoration_retains_exact_output_and_denies_copies_before_allocation() {
    let (schema, _, _) = fixture();
    let owner = MemoryBudget::new(4 << 20);
    let names = names(&row(), &owner);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut restored = None;
    let allocated = allocation_counter::measure(|| {
        restored = Some(
            names
                .decode_with_control(&schema, &memory, &token, &token)
                .unwrap(),
        );
    });
    assert_eq!(
        usize::try_from(allocated.bytes_current).unwrap(),
        memory.used()
    );
    assert!(usize::try_from(allocated.bytes_max).unwrap() <= memory.peak());
    drop((restored, names));
    assert_eq!(memory.used(), 0);
    assert_eq!(owner.used(), 0);
    let mut large = row();
    large.relation.name = "x".repeat(1 << 20);
    let names = super::names(&large, &owner);
    let retained = owner.used();
    let memory = MemoryBudget::new(16384);
    let allocation = allocation_counter::measure(|| {
        assert!(matches!(
            names.decode_with_control(&schema, &memory, &token, &token),
            Err(StorageBackendError::Memory(_))
        ));
    });
    assert!(allocation.bytes_total < 32768, "{allocation:?}");
    assert_eq!(memory.used(), 0);
    assert_eq!(owner.used(), retained);
    drop(names);
    assert_eq!(owner.used(), 0);
}
