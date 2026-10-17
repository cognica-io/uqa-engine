//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_pg_regex::{REG_ADVANCED, REG_ICASE};

const DEFAULT_COLLATION: u32 = crate::catalog::collations::DEFAULT_COLLATION_OID as u32;

#[test]
fn compiled_expressions_charge_and_return_every_engine_byte() {
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let call = RegexCall::new(&control);
    let mut allowance = call.allowance();
    let mut regex = call
        .compile(
            "(a|ab)(c|bcd)(d*)",
            REG_ADVANCED,
            DEFAULT_COLLATION,
            &mut allowance,
        )
        .unwrap();
    assert!(budget.used() > 0, "the compiled state is charged");
    let input = code_points("abcd", &control).unwrap();
    let mut spans = [MatchSpan::UNSET; 4];
    assert!(call.execute(&mut regex, &input, 0, &mut spans).unwrap());
    assert_eq!(spans[0].range(), Some(0..4));
    drop(regex);
    drop(allowance);
    drop(input);
    assert_eq!(budget.used(), 0);
}

#[test]
fn an_exhausted_allowance_is_reported_as_the_allowance_error() {
    // Growing limits refuse compilation at every point from the wrapper's first reservation through the engine's allocations until it succeeds; each refusal reports the allowance's error and returns every byte.
    let mut limit = 0;
    loop {
        let budget = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let call = RegexCall::new(&control);
        let mut allowance = call.allowance();
        let refused = match call.compile(
            "(a|b)*c{1,20}",
            REG_ADVANCED,
            DEFAULT_COLLATION,
            &mut allowance,
        ) {
            Ok(regex) => {
                drop(regex);
                false
            }
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"), "{error}");
                assert!(
                    error
                        .to_string()
                        .ends_with(&format!("exceeding limit {limit}")),
                    "{error}"
                );
                true
            }
        };
        drop(allowance);
        assert_eq!(budget.used(), 0, "{limit}");
        if !refused {
            break;
        }
        limit += 512;
    }
    assert!(limit > 0);
}

#[test]
fn refusals_take_precedence_over_the_engine_out_of_memory_report() {
    let control = ProductionControl::uncontrolled();
    let call = RegexCall::new(&control);
    for error in [RegexError::Code(REG_ESPACE), RegexError::OutOfMemory] {
        call.refuse(MemoryError::Limit {
            required: 2048,
            limit: 1024,
        });
        let reported = call.failure(error, "invalid regular expression");
        assert_eq!(reported.sqlstate(), Some("53200"));
        assert_eq!(
            reported.to_string(),
            "memory requires 2048 bytes, exceeding limit 1024"
        );
    }
    // Without a refusal the engine's own reports stand, as `PostgreSQL` raises them.
    let reported = call.failure(RegexError::Code(REG_ESPACE), "invalid regular expression");
    assert_eq!(reported.sqlstate(), Some("2201B"));
    assert_eq!(
        reported.to_string(),
        "invalid regular expression: out of memory"
    );
    let reported = call.failure(RegexError::OutOfMemory, "invalid regular expression");
    assert_eq!(reported.sqlstate(), Some("53200"));
    assert_eq!(reported.to_string(), "out of memory");
}

#[test]
fn engine_interrupt_checks_report_the_statement_cancellation() {
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let call = RegexCall::new(&control);
    let mut allowance = call.allowance();
    token.cancel();
    // Compile through the engine directly, past the call's own check, so the engine's interrupt hook observes the cancellation.
    let collation = RegexCollation::from_oid(DEFAULT_COLLATION).unwrap();
    let Err(error) = Regex::compile("(a|b)*c", REG_ADVANCED, collation, &mut allowance) else {
        panic!("a canceled compilation fails")
    };
    assert_eq!(error, RegexError::Cancelled);
    assert_eq!(
        call.failure(error, "invalid regular expression").sqlstate(),
        Some("57014")
    );
    drop(allowance);
    assert_eq!(budget.used(), 0);
}

#[test]
fn the_running_statements_cancellation_reaches_the_engine() {
    // Ordinary evaluation, planning and table functions call the engine without a cancellation scope of their own, so the engine observes the running statement's.
    let token = CancellationToken::new();
    let _statement = crate::expr::StatementCancellationScope::enter(token.clone());
    let control = ProductionControl::uncontrolled();
    let call = RegexCall::new(&control);
    let mut allowance = call.allowance();
    token.cancel();
    let collation = RegexCollation::from_oid(DEFAULT_COLLATION).unwrap();
    let Err(error) = Regex::compile("(a|b)*c", REG_ADVANCED, collation, &mut allowance) else {
        panic!("a canceled statement stops the engine")
    };
    assert_eq!(error, RegexError::Cancelled);
    assert_eq!(
        call.failure(error, "invalid regular expression").sqlstate(),
        Some("57014")
    );
    let Err(error) = call.compile("a", REG_ADVANCED, DEFAULT_COLLATION, &mut allowance) else {
        panic!("a canceled statement stops the call between engine operations")
    };
    assert_eq!(error.sqlstate(), Some("57014"));
}

#[test]
fn collations_are_checked_before_the_pattern() {
    let control = ProductionControl::uncontrolled();
    let call = RegexCall::new(&control);
    let mut allowance = call.allowance();
    let Err(SQLError::Diagnostic {
        sqlstate,
        message,
        hint,
        ..
    }) = call.compile("(", REG_ADVANCED, 0, &mut allowance)
    else {
        panic!("an indeterminate collation is a diagnostic")
    };
    assert_eq!(sqlstate, "42P22");
    assert_eq!(
        message,
        "could not determine which collation to use for regular expression"
    );
    assert_eq!(
        hint.as_deref(),
        Some("Use the COLLATE clause to set the collation explicitly.")
    );
    let Err(error) = call.compile("(", REG_ADVANCED, DEFAULT_COLLATION, &mut allowance) else {
        panic!("an unbalanced parenthesis is invalid")
    };
    assert_eq!(error.sqlstate(), Some("2201B"));
    assert_eq!(
        error.to_string(),
        "invalid regular expression: parentheses () not balanced"
    );
}

#[test]
fn the_collation_selects_classes_and_case_folding() {
    let control = ProductionControl::uncontrolled();
    let call = RegexCall::new(&control);
    let latin_small_e_acute = code_points("\u{e9}", &control).unwrap();
    for (collation, alpha, folded) in [(950, false, false), (811, true, true), (6411, true, true)] {
        let mut allowance = call.allowance();
        let mut regex = call
            .compile("^[[:alpha:]]$", REG_ADVANCED, collation, &mut allowance)
            .unwrap();
        assert_eq!(
            call.execute(&mut regex, &latin_small_e_acute, 0, &mut [])
                .unwrap(),
            alpha,
            "{collation}"
        );
        drop(regex);
        let mut regex = call
            .compile(
                "\u{c9}",
                REG_ADVANCED | REG_ICASE,
                collation,
                &mut allowance,
            )
            .unwrap();
        assert_eq!(
            call.execute(&mut regex, &latin_small_e_acute, 0, &mut [])
                .unwrap(),
            folded,
            "{collation}"
        );
    }
}
