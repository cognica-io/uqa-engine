//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn admitted_range_text_preserves_discrete_numeric_temporal_and_empty_canonicalization() {
    let memory = MemoryBudget::new(1024 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for (input, subtype, expected) in [
        ("(1,4]", RangeSubtype::Integer, "[2,5)"),
        (
            "(2147483648,2147483650]",
            RangeSubtype::BigInteger,
            "[2147483649,2147483651)",
        ),
        ("[1.00,2.000)", RangeSubtype::Numeric, "[1.00,2.000)"),
        (
            "[2024-01-01,2024-01-02]",
            RangeSubtype::Date,
            "[2024-01-01,2024-01-03)",
        ),
        (
            "[\"2024-01-01 00:00:00\",\"2024-01-02 01:02:03\")",
            RangeSubtype::Timestamp,
            "[\"2024-01-01 00:00:00\",\"2024-01-02 01:02:03\")",
        ),
        ("(,)", RangeSubtype::TimestampTz, "(,)"),
        ("[1,1)", RangeSubtype::Numeric, "empty"),
        (" EMPTY ", RangeSubtype::Integer, "empty"),
    ] {
        let output = canonical_range_text_with_control(input, subtype, &control).unwrap();
        assert_eq!(&**output, expected);
        assert_eq!(memory.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(memory.used(), 0);
        let multi =
            canonical_range_as_multirange_text_with_control(input, subtype, &control).unwrap();
        assert_eq!(
            &**multi,
            if expected == "empty" {
                "{}".into()
            } else {
                format!("{{{expected}}}")
            }
        );
        drop(multi);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn admitted_multiranges_merge_in_stable_bound_order_without_cloning_numeric_payloads() {
    let memory = MemoryBudget::new(1024 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    for (input, subtype, expected) in [
        (
            "{[10,12),[1,3),[3,5)}",
            RangeSubtype::Integer,
            "{[1,5),[10,12)}",
        ),
        (
            "{[1.00,2.000),[1.0,3.00),[3,4.0)}",
            RangeSubtype::Numeric,
            "{[1.00,4.0)}",
        ),
        ("{(1,2),(2,3)}", RangeSubtype::Numeric, "{(1,2),(2,3)}"),
        ("{(1,2],[2,3)}", RangeSubtype::Numeric, "{(1,3)}"),
        ("{[3,3),[1,2),[2,2)}", RangeSubtype::Integer, "{[1,2)}"),
        ("{(,2),[2,)}", RangeSubtype::Numeric, "{(,)}"),
        ("{}", RangeSubtype::Date, "{}"),
    ] {
        let output = canonical_multirange_text_with_control(input, subtype, &control).unwrap();
        assert_eq!(&**output, expected);
        assert_eq!(memory.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn admitted_range_quota_cancellation_and_parse_failures_leave_existing_owners_intact() {
    for allowance in [8, 32, 128, 512, 2048, 16384] {
        let memory = MemoryBudget::new(allowance);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let prior = control.copy_text("prior").unwrap();
        let before = memory.used();
        match canonical_multirange_text_with_control(
            "{[1.000,2.00),[2,3.000)}",
            RangeSubtype::Numeric,
            &control,
        ) {
            Ok(output) => drop(output),
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(memory.used(), before);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                canonical_range_text_with_control("[1,2)", RangeSubtype::Integer, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(
                canonical_multirange_text_with_control("{}", RangeSubtype::Integer, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(
                canonical_range_as_multirange_text_with_control(
                    "empty",
                    RangeSubtype::Integer,
                    &control
                )
                .unwrap_err()
                .sqlstate(),
                Some("57014")
            );
            token.reset();
        }
        assert_eq!(memory.used(), before);
        assert_eq!(&**prior, "prior");
        drop(prior);
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    for (input, subtype, state) in [
        ("[1,nope)", RangeSubtype::Numeric, "22P02"),
        ("[2147483647,2147483647]", RangeSubtype::Integer, "22003"),
    ] {
        assert_eq!(
            canonical_range_text_with_control(input, subtype, &control)
                .unwrap_err()
                .sqlstate(),
            Some(state)
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn range_workspace_releases_values_before_their_lease_on_error_and_unwind() {
    use std::cell::Cell;

    struct DropWitness<'a> {
        budget: &'a MemoryBudget,
        observed: &'a Cell<bool>,
    }
    impl Drop for DropWitness<'_> {
        fn drop(&mut self) {
            self.observed.set(self.budget.used() != 0);
        }
    }
    let memory = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancellation, &cancellation);
    let observed = Cell::new(false);
    let make_owner = || {
        let mut values = ProductionVec::new(control);
        values
            .push_produced(
                control
                    .finish(
                        DropWitness {
                            budget: &memory,
                            observed: &observed,
                        },
                        control.empty_reservation(),
                    )
                    .unwrap(),
            )
            .unwrap();
        RangeWorkspace::from_produced(values.finish().unwrap())
    };
    let failed = (|| -> std::result::Result<(), ()> {
        let _owner = make_owner();
        let result = std::result::Result::<(), ()>::Err(());
        result?;
        Ok(())
    })();
    assert!(failed.is_err());
    assert!(observed.get());
    assert_eq!(memory.used(), 0);
    observed.set(false);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _owner = make_owner();
        panic!("injected range workspace failure");
    }));
    assert!(unwound.is_err());
    assert!(observed.get());
    assert_eq!(memory.used(), 0);
}
