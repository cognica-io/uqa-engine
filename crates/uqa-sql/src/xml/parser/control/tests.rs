//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::xml::{validate_with_control, XmlOption};
use std::cell::RefCell;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[derive(Default)]
struct Fault {
    calls: usize,
    fail_at: Option<usize>,
    reads: usize,
    cancel_at: Option<(usize, CancellationToken)>,
}

thread_local! { static FAULT: RefCell<Fault> = RefCell::default(); }

pub(super) fn allocation(bytes: usize) -> Result<(), ValueRetentionError> {
    FAULT.with_borrow_mut(|fault| {
        if bytes == 0 {
            fault.reads += 1;
            if let Some((index, token)) = &fault.cancel_at {
                if fault.reads == *index {
                    token.cancel();
                }
            }
        } else {
            fault.calls += 1;
            if fault.fail_at == Some(fault.calls) {
                return Err(MemoryError::Limit {
                    required: bytes,
                    limit: 0,
                }
                .into());
            }
        }
        Ok(())
    })
}

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        FAULT.with_borrow_mut(|fault| *fault = Fault::default());
    }
}

#[test]
fn every_native_allocation_failure_releases_the_parser_and_retained_diagnostics() {
    let _reset = Reset;
    let budget = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (input, option) in [
        ("<a/>text<b/>", XmlOption::Content),
        (
            "<!DOCTYPE a [<!ENTITY e 'value'>]><a x='attribute'>&e;</a>",
            XmlOption::Document,
        ),
        ("<?xml version='1.1'?><a/>", XmlOption::Document),
        ("<a><b></a>", XmlOption::Document),
    ] {
        FAULT.with_borrow_mut(|fault| *fault = Fault::default());
        let _ = validate_with_control(input, option, &control, |_| {});
        let calls = FAULT.with_borrow(|fault| fault.calls);
        assert!(calls > 10);
        assert_eq!(budget.used(), 0);
        for fail_at in 1..=calls {
            FAULT.with_borrow_mut(|fault| {
                *fault = Fault {
                    fail_at: Some(fail_at),
                    ..Fault::default()
                }
            });
            let error = validate_with_control(input, option, &control, |_| {}).unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some("53200"),
                "allocation {fail_at}: {error}"
            );
            assert_eq!(budget.used(), 0, "allocation {fail_at}");
        }
    }
}

#[test]
fn native_parsing_stops_for_each_cancellation_owner_during_bounded_input_reads() {
    let _reset = Reset;
    let input = format!("<a>{}</a>", "ordinary text ".repeat(20_000));
    for original_owner in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let budget = MemoryBudget::new(4 << 20);
        let token = if original_owner {
            original.clone()
        } else {
            invoking.clone()
        };
        FAULT.with_borrow_mut(|fault| {
            *fault = Fault {
                cancel_at: Some((4, token)),
                ..Fault::default()
            }
        });
        let error = validate_with_control(
            &input,
            XmlOption::Document,
            &ProductionControl::new(&budget, &original, &invoking),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
        assert_eq!(FAULT.with_borrow(|fault| fault.reads), 4);
    }
}

#[test]
fn native_quota_failure_and_notice_unwind_leave_no_allocation_or_thread_scope() {
    let token = CancellationToken::new();
    for limit in [0, 1024, 4096, 16_384] {
        let budget = MemoryBudget::new(limit);
        let input = format!("<a>{}</a>", "payload ".repeat(32_000));
        let error = validate_with_control(
            &input,
            XmlOption::Content,
            &ProductionControl::new(&budget, &token, &token),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(4 << 20);
    let control = ProductionControl::new(&budget, &token, &token);
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        validate_with_control(
            "<?xml version='1.1'?><a/>",
            XmlOption::Document,
            &control,
            |_| {
                let nested = MemoryBudget::new(1 << 20);
                validate_with_control(
                    "<inner/>",
                    XmlOption::Document,
                    &ProductionControl::new(&nested, &token, &token),
                    |_| {},
                )
                .unwrap();
                assert_eq!(nested.used(), 0);
                panic!("notice consumer failure");
            },
        )
    }));
    assert!(failure.is_err());
    assert_eq!(budget.used(), 0);
    validate_with_control("<a/>", XmlOption::Document, &control, |_| {}).unwrap();
    assert_eq!(budget.used(), 0);
}

#[test]
fn concurrent_native_parsers_keep_independent_allowances_and_faults() {
    std::thread::scope(|scope| {
        for failing in [false, true, false, true] {
            scope.spawn(move || {
                let token = CancellationToken::new();
                let budget = MemoryBudget::new(if failing { 0 } else { 1 << 20 });
                for _ in 0..16 {
                    let result = validate_with_control(
                        "<a>text</a>",
                        XmlOption::Document,
                        &ProductionControl::new(&budget, &token, &token),
                        |_| {},
                    );
                    assert_eq!(result.is_err(), failing);
                    assert_eq!(budget.used(), 0);
                }
            });
        }
    });
}
