//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_input_metadata_application_retires_every_prefix_and_both_cancellation_scopes() {
    let statement = syntax("SELECT original FROM old_source");
    let replacement = syntax(
        "SELECT renamed,(SELECT nested) FROM ROWS FROM(generate_series(1,2), generate_series(3,4))",
    );
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (plan, constants, observed) =
        prepare_native(&statement, Some(&replacement), &native, &work, &token);
    let mut decoder = input(
        plan,
        constants,
        &native,
        &work,
        &token,
        &token,
        Arc::default(),
    );
    let mut pending = 0;
    loop {
        if let Some(output) = advance(&mut decoder).unwrap() {
            drop(output);
            break;
        }
        pending += 1;
        assert!(pending < 10_000);
    }
    assert!(pending > 20);
    drop((decoder, observed));
    assert_eq!((native.used(), work.used()), (0, 0));
    for prefix in 0..=pending {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let sibling = native.reserve(37).unwrap();
            let (plan, constants, observed) =
                prepare_native(&statement, Some(&replacement), &native, &work, &original);
            let mut decoder = input(
                plan,
                constants,
                &native,
                &work,
                &original,
                &invoking,
                Arc::default(),
            );
            for _ in 0..prefix {
                assert!(advance(&mut decoder).unwrap().is_none());
            }
            match cancellation {
                1 => original.cancel(),
                2 => invoking.cancel(),
                _ => (),
            }
            if cancellation != 0 {
                assert!(advance(&mut decoder).is_err());
            }
            let cleanup = allocation_counter::measure(|| drop((decoder, observed)));
            assert_eq!(
                cleanup.count_total, 0,
                "prefix {prefix}, cancellation {cancellation}"
            );
            assert_eq!((native.used(), work.used()), (37, 0));
            drop(sibling);
            assert_eq!(native.used(), 0);
        }
    }
}

#[test]
fn native_input_metadata_application_releases_observations_after_denied_admission() {
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let sibling = native.reserve(37).unwrap();
    let (plan, constants, observed) = prepare_native(
        &syntax("SELECT original FROM old_source"),
        Some(&syntax("SELECT renamed FROM generate_series(1,2)")),
        &native,
        &work,
        &token,
    );
    let mut decoder = input(
        plan,
        constants,
        &native,
        &work,
        &token,
        &token,
        Arc::default(),
    );
    let held = native.reserve(native.available()).unwrap();
    let error = loop {
        match advance(&mut decoder) {
            Err(error) => break error,
            Ok(None) => (),
            Ok(Some(_)) => panic!("exhausted destination accepted replacement"),
        }
    };
    assert_eq!(error.sqlstate(), Some("53200"));
    drop((error, decoder, observed, held));
    assert_eq!((native.used(), work.used()), (37, 0));
    drop(sibling);
    assert_eq!(native.used(), 0);
}
