//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Memory admission, cancellation, hook panics and stack limits.
//!
//! The fault-injection tests first count the reservations or interrupt checks of an operation and then fail each one in turn, so every allocation and interrupt site the operation reaches is exercised.

use std::panic::{self, AssertUnwindSafe};

use uqa_pg_regex::{
    MatchSpan, Regex, RegexCollation, RegexError, RegexResources, REG_ADVANCED, REG_ESPACE,
    REG_ETOOBIG, REG_ICASE,
};

use crate::support::{code_points, Budget, SharedBudget};

/// Lookbehind, classes, a bound, an alternation, a back-reference, a word boundary and a lookahead.
const COMPILE_PATTERN: &str = r"(?<=x)([[:alpha:]]+?)(\d{2,4}|[[:punct:]])\1\y(?=z|$)";

/// More than 20 subexpressions with a back-reference and a lookbehind, so that `pg_regexec` allocates its match work area, its subexpression DFAs and its lookbehind state.
const EXEC_PATTERN: &str =
    r"(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)(l)(m)(n)(o)(p)(q)(r)(s)(t)(u)(?<=u)v\1";
const EXEC_INPUT: &str = "zzabcdefghijklmnopqrstuva";

fn compile<'r>(
    pattern: &str,
    resources: &'r mut dyn RegexResources,
) -> Result<Regex<'r>, RegexError> {
    Regex::compile(
        pattern,
        REG_ADVANCED | REG_ICASE,
        RegexCollation::Icu,
        resources,
    )
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or_default()
}

#[test]
fn compiled_state_is_charged_and_dropping_releases_every_byte() {
    let shared = SharedBudget::default();
    let mut hooks = shared.clone();
    let mut regex = compile(COMPILE_PATTERN, &mut hooks).unwrap();
    let compiled = shared.0.borrow().live;
    assert!(compiled > 0);
    assert_eq!(compiled, regex.reserved_bytes());
    let mut spans = [MatchSpan::UNSET; 3];
    assert!(regex.exec(&code_points("xAb12ab"), 0, &mut spans).unwrap());
    assert_eq!(spans[0].range(), Some(1..7));
    // The execution's working memory is gone again; only the compiled state remains.
    assert_eq!(shared.0.borrow().live, compiled);
    assert!(shared.0.borrow().peak > compiled);
    drop(regex);
    assert_eq!(shared.0.borrow().live, 0);
}

#[test]
fn every_refused_compile_reservation_reports_out_of_memory_without_leaking() {
    let mut probe = Budget::unlimited();
    drop(compile(COMPILE_PATTERN, &mut probe).unwrap());
    let reservations = probe.reservations;
    assert!(reservations > 20, "{reservations}");
    for index in 0..reservations {
        let mut budget = Budget {
            refuse_reservation: Some(index),
            ..Budget::default()
        };
        let error = compile(COMPILE_PATTERN, &mut budget)
            .err()
            .unwrap_or_else(|| panic!("compilation ignored refused reservation {index}"));
        // The expression's memory context and the converted pattern come first; PostgreSQL's context creation and palloc raise "out of memory" for them, while the engine reports REG_ESPACE.
        if index < 2 {
            assert_eq!(error, RegexError::OutOfMemory, "reservation {index}");
            assert_eq!(error.sqlstate(), Some("53200"));
        } else {
            assert_eq!(error.code(), Some(REG_ESPACE), "reservation {index}");
        }
        assert_eq!(error.message(), "out of memory");
        assert_eq!(budget.live, 0, "reservation {index}");
    }
}

#[test]
fn a_byte_limit_fails_compilation_with_out_of_memory() {
    let pattern = "(((a{1,100}){1,100}){1,100})";
    let mut budget = Budget {
        limit: Some(256 * 1024),
        ..Budget::default()
    };
    let error = compile(pattern, &mut budget).err().unwrap();
    assert_eq!(error.code(), Some(REG_ESPACE));
    assert!(budget.peak <= 256 * 1024);
    assert_eq!(budget.live, 0);
}

#[test]
fn every_refused_execution_reservation_reports_out_of_memory_and_keeps_the_expression() {
    let input = code_points(EXEC_INPUT);
    let shared = SharedBudget::default();
    let mut hooks = shared.clone();
    let mut regex =
        Regex::compile(EXEC_PATTERN, REG_ADVANCED, RegexCollation::C, &mut hooks).unwrap();
    let compiled = shared.0.borrow().live;
    let mut spans = [MatchSpan::UNSET; 2];
    let before = shared.0.borrow().reservations;
    assert!(regex.exec(&input, 0, &mut spans).unwrap());
    let expected = spans;
    assert_eq!(expected[0].range(), Some(2..25));
    let reservations = shared.0.borrow().reservations - before;
    assert!(reservations >= 4, "{reservations}");
    for offset in 0..reservations {
        let index = shared.0.borrow().reservations + offset;
        shared.0.borrow_mut().refuse_reservation = Some(index);
        let error = regex.exec(&input, 0, &mut spans).unwrap_err();
        assert_eq!(error.code(), Some(REG_ESPACE), "reservation {offset}");
        assert_eq!(spans, [MatchSpan::UNSET; 2]);
        assert_eq!(shared.0.borrow().live, compiled, "reservation {offset}");
        shared.0.borrow_mut().refuse_reservation = None;
        assert!(regex.exec(&input, 0, &mut spans).unwrap());
        assert_eq!(spans, expected);
    }
    drop(regex);
    assert_eq!(shared.0.borrow().live, 0);
}

#[test]
fn compilation_stops_at_every_interrupt_check() {
    let mut probe = Budget::unlimited();
    drop(compile(COMPILE_PATTERN, &mut probe).unwrap());
    let checks = probe.interrupts;
    assert!(checks > 10, "{checks}");
    for index in 0..checks {
        let mut budget = Budget {
            cancel_interrupt: Some(index),
            ..Budget::default()
        };
        let error = compile(COMPILE_PATTERN, &mut budget).err().unwrap();
        assert_eq!(error, RegexError::Cancelled, "check {index}");
        assert_eq!(budget.live, 0, "check {index}");
    }
}

#[test]
fn execution_stops_at_every_interrupt_check_and_keeps_the_expression() {
    let input = code_points(EXEC_INPUT);
    let shared = SharedBudget::default();
    let mut hooks = shared.clone();
    let mut regex =
        Regex::compile(EXEC_PATTERN, REG_ADVANCED, RegexCollation::C, &mut hooks).unwrap();
    let compiled = shared.0.borrow().live;
    let mut spans = [MatchSpan::UNSET; 1];
    let before = shared.0.borrow().interrupts;
    assert!(regex.exec(&input, 0, &mut spans).unwrap());
    let checks = shared.0.borrow().interrupts - before;
    assert!(checks > 2, "{checks}");
    for offset in 0..checks {
        let index = shared.0.borrow().interrupts + offset;
        shared.0.borrow_mut().cancel_interrupt = Some(index);
        assert_eq!(
            regex.exec(&input, 0, &mut spans),
            Err(RegexError::Cancelled),
            "check {offset}"
        );
        assert_eq!(spans, [MatchSpan::UNSET]);
        assert_eq!(shared.0.borrow().live, compiled);
        shared.0.borrow_mut().cancel_interrupt = None;
        assert!(regex.exec(&input, 0, &mut spans).unwrap());
    }
    drop(regex);
    assert_eq!(shared.0.borrow().live, 0);
}

#[test]
fn a_panicking_hook_unwinds_after_the_operation_frees_its_memory() {
    let mut probe = Budget::unlimited();
    drop(compile(COMPILE_PATTERN, &mut probe).unwrap());
    for index in [0, 1, 2, probe.reservations / 2, probe.reservations - 1] {
        let mut budget = Budget {
            panic_reservation: Some(index),
            ..Budget::default()
        };
        let payload = panic::catch_unwind(AssertUnwindSafe(|| {
            drop(compile(COMPILE_PATTERN, &mut budget));
        }))
        .unwrap_err();
        assert_eq!(
            panic_message(payload.as_ref()),
            format!("reservation hook panic injected at {index}")
        );
        assert_eq!(budget.live, 0, "reservation {index}");
    }

    let input = code_points(EXEC_INPUT);
    let shared = SharedBudget::default();
    let mut hooks = shared.clone();
    let mut regex =
        Regex::compile(EXEC_PATTERN, REG_ADVANCED, RegexCollation::C, &mut hooks).unwrap();
    let compiled = shared.0.borrow().live;
    let index = shared.0.borrow().interrupts + 1;
    shared.0.borrow_mut().panic_interrupt = Some(index);
    let payload = panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = regex.exec(&input, 0, &mut [MatchSpan::UNSET]);
    }))
    .unwrap_err();
    assert_eq!(
        panic_message(payload.as_ref()),
        format!("interrupt hook panic injected at {index}")
    );
    assert_eq!(shared.0.borrow().live, compiled);
    shared.0.borrow_mut().panic_interrupt = None;
    assert!(regex.exec(&input, 0, &mut []).unwrap());
    drop(regex);
    assert_eq!(shared.0.borrow().live, 0);
}

/// Hooks whose release panics, after accounting the returned bytes, when the last reserved byte comes back; only dropping an expression returns it.
#[derive(Default)]
struct PanickingRelease {
    budget: Budget,
}

impl RegexResources for PanickingRelease {
    fn reserve(&mut self, bytes: usize) -> bool {
        self.budget.reserve(bytes)
    }

    fn release(&mut self, bytes: usize) {
        self.budget.release(bytes);
        assert!(self.budget.live != 0, "release hook panic");
    }

    fn cancel_requested(&mut self) -> bool {
        false
    }

    fn stack_too_deep(&mut self, depth: usize) -> bool {
        self.budget.stack_too_deep(depth)
    }
}

#[test]
fn dropping_resumes_a_release_panic_unless_the_thread_is_unwinding() {
    let mut hooks = PanickingRelease::default();
    let regex =
        Regex::compile("[[:alpha:]]+", REG_ADVANCED, RegexCollation::C, &mut hooks).unwrap();
    let payload = panic::catch_unwind(AssertUnwindSafe(|| drop(regex))).unwrap_err();
    assert_eq!(panic_message(payload.as_ref()), "release hook panic");
    assert_eq!(hooks.budget.live, 0);

    // Dropping during unwinding would otherwise panic twice and abort the process.
    let mut hooks = PanickingRelease::default();
    let payload = panic::catch_unwind(AssertUnwindSafe(|| {
        let _regex =
            Regex::compile("[[:alpha:]]+", REG_ADVANCED, RegexCollation::C, &mut hooks).unwrap();
        panic!("operation panic");
    }))
    .unwrap_err();
    assert_eq!(panic_message(payload.as_ref()), "operation panic");
    assert_eq!(hooks.budget.live, 0);
}

#[test]
fn a_deep_stack_reports_regular_expression_too_complex() {
    let pattern = format!("{}a{}", "(".repeat(200), ")".repeat(200));
    let input = code_points("xa");
    let shared = SharedBudget::default();
    let mut hooks = shared.clone();
    let mut regex = Regex::compile(&pattern, REG_ADVANCED, RegexCollation::C, &mut hooks).unwrap();
    // Frame sizes differ between targets, so the limits below are set from the depths actually reached.
    let compile_depth = shared.0.borrow().deepest;
    assert!(compile_depth > 0);
    assert_eq!(regex.subexpression_count(), 200);
    let mut spans = vec![MatchSpan::UNSET; 201];
    shared.0.borrow_mut().deepest = 0;
    assert!(regex.exec(&input, 0, &mut spans).unwrap());
    assert!(spans.iter().all(|span| span.range() == Some(1..2)));
    let exec_depth = shared.0.borrow().deepest;
    assert!(exec_depth > 0);
    // Matching recurses through the subexpression tree under the same check.
    shared.0.borrow_mut().stack_limit = Some(exec_depth / 2);
    let error = regex.exec(&input, 0, &mut spans).unwrap_err();
    assert_eq!(error.code(), Some(REG_ETOOBIG));
    shared.0.borrow_mut().stack_limit = None;
    assert!(regex.exec(&input, 0, &mut spans).unwrap());
    drop(regex);
    assert_eq!(shared.0.borrow().live, 0);

    let mut budget = Budget {
        stack_limit: Some(compile_depth / 2),
        ..Budget::default()
    };
    let error = Regex::compile(&pattern, REG_ADVANCED, RegexCollation::C, &mut budget)
        .err()
        .unwrap();
    assert_eq!(error.code(), Some(REG_ETOOBIG));
    assert_eq!(error.message(), "regular expression is too complex");
    assert_eq!(budget.live, 0);
}

/// Hooks that run a complete operation of their own, under another collation, from inside the engine.
#[derive(Default)]
struct Nesting {
    budget: Budget,
    nested: bool,
}

impl RegexResources for Nesting {
    fn reserve(&mut self, bytes: usize) -> bool {
        self.budget.reserve(bytes)
    }

    fn release(&mut self, bytes: usize) {
        self.budget.release(bytes);
    }

    fn cancel_requested(&mut self) -> bool {
        if !self.nested {
            self.nested = true;
            let mut inner = Budget::unlimited();
            let mut regex = Regex::compile(
                "[[:alpha:]]+",
                REG_ADVANCED,
                RegexCollation::BuiltinUnicodeFast,
                &mut inner,
            )
            .unwrap();
            let mut spans = [MatchSpan::UNSET];
            assert!(regex
                .exec(&code_points("1\u{e9}\u{e8}2"), 0, &mut spans)
                .unwrap());
            assert_eq!(spans[0].range(), Some(1..3));
            drop(regex);
            assert_eq!(inner.live, 0);
        }
        self.budget.cancel_requested()
    }

    fn stack_too_deep(&mut self, depth: usize) -> bool {
        self.budget.stack_too_deep(depth)
    }
}

#[test]
fn a_hook_may_run_another_operation_with_its_own_locale() {
    let mut hooks = Nesting::default();
    {
        let mut regex =
            Regex::compile("[[:alpha:]]+", REG_ADVANCED, RegexCollation::C, &mut hooks).unwrap();
        let mut spans = [MatchSpan::UNSET];
        // The outer expression keeps the C locale: the accented letters are not alphabetic.
        assert!(regex.exec(&code_points("\u{e9}ab"), 0, &mut spans).unwrap());
        assert_eq!(spans[0].range(), Some(1..3));
    }
    assert!(hooks.nested);
    assert_eq!(hooks.budget.live, 0);
}

#[cfg(not(target_family = "wasm"))]
#[test]
fn concurrent_operations_on_different_threads_are_independent() {
    let expectations = [
        (RegexCollation::C, false),
        (RegexCollation::BuiltinCUtf8, true),
        (RegexCollation::BuiltinUnicodeFast, true),
        (RegexCollation::Icu, true),
    ];
    std::thread::scope(|scope| {
        for (collation, folds) in expectations {
            for _ in 0..2 {
                scope.spawn(move || {
                    for _ in 0..200 {
                        let mut budget = Budget::unlimited();
                        let mut regex = Regex::compile(
                            "\u{e9}[[:alpha:]]",
                            REG_ADVANCED | REG_ICASE,
                            collation,
                            &mut budget,
                        )
                        .unwrap();
                        let matched = regex
                            .exec(&code_points("\u{c9}\u{e8}"), 0, &mut [])
                            .unwrap();
                        assert_eq!(matched, folds, "{collation:?}");
                        drop(regex);
                        assert_eq!(budget.live, 0);
                    }
                });
            }
        }
    });
}
