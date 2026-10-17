//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The engine's call depth under Emscripten.
//!
//! The engine recurses on the WebAssembly call stack there, which the depth checks bound by counting the engine's live frames against a budget of half of V8's default stack. Every shape below recurses in proportion to its size; beyond the budget it fails with `REG_ETOOBIG` and leaves the module usable, and within it it succeeds.
#![cfg(target_os = "emscripten")]

use uqa_pg_regex::{
    MatchSpan, Regex, RegexCollation, RegexError, RegexResources, MAX_STACK_DEPTH, REG_ADVANCED,
    REG_ETOOBIG,
};

use crate::support::{code_points, Budget};

/// `count` groups opened with `opening` around `a`.
fn nested(opening: &str, count: usize) -> String {
    format!("{}a{}", opening.repeat(count), ")".repeat(count))
}

/// `count` capturing groups followed by a back-reference to the first.
fn back_reference(count: usize) -> String {
    format!("{}\\1", "(a)".repeat(count))
}

/// Compile `pattern` and match `subject` with hooks that set no limit of their own.
fn compile_and_match(pattern: &str, subject: &str) -> Result<bool, RegexError> {
    let mut budget = Budget::unlimited();
    let outcome = Regex::compile(pattern, REG_ADVANCED, RegexCollation::C, &mut budget).and_then(
        |mut regex| {
            let mut spans = vec![MatchSpan::UNSET; regex.subexpression_count() + 1];
            regex.exec(&code_points(subject), 0, &mut spans)
        },
    );
    assert_eq!(budget.live, 0);
    outcome
}

fn assert_ordinary_operations_work() {
    assert_eq!(compile_and_match(r"(\d+)-(\d+)", "tel 555-0100"), Ok(true));
}

#[test]
fn recursion_beyond_the_frame_budget_reports_regular_expression_too_complex() {
    let shapes = [
        (
            "nested capturing groups",
            nested("(", 400),
            String::from("a"),
        ),
        (
            "nested non-capturing groups",
            nested("(?:", 500),
            String::from("a"),
        ),
        ("a long literal", "a".repeat(2000), String::new()),
        // Twelve characters that expand into a chain of about 10200 states.
        (
            "a bounded repetition chain",
            String::from("(a{255}){40}"),
            String::new(),
        ),
        ("nested lookaheads", nested("(?=", 1000), String::from("a")),
        (
            "a back-reference after many groups",
            back_reference(600),
            "a".repeat(601),
        ),
    ];
    for (name, pattern, subject) in shapes {
        let error = compile_and_match(&pattern, &subject).expect_err(name);
        assert_eq!(error, RegexError::Code(REG_ETOOBIG), "{name}");
        assert_eq!(error.message(), "regular expression is too complex");
        assert_ordinary_operations_work();
    }
}

#[test]
fn recursion_within_the_frame_budget_succeeds() {
    let shapes = [
        (
            "nested capturing groups",
            nested("(", 250),
            String::from("a"),
        ),
        (
            "nested non-capturing groups",
            nested("(?:", 300),
            String::from("a"),
        ),
        ("a long literal", "a".repeat(1000), "a".repeat(1000)),
        (
            "a bounded repetition chain",
            String::from("(a{255}){4}"),
            "a".repeat(1020),
        ),
        ("nested lookaheads", nested("(?=", 600), String::from("a")),
        (
            "a back-reference after many groups",
            back_reference(400),
            "a".repeat(401),
        ),
    ];
    for (name, pattern, subject) in shapes {
        assert_eq!(compile_and_match(&pattern, &subject), Ok(true), "{name}");
    }
}

/// Hooks whose depth check matches another expression once the operation is deep, so that the frames of both operations are live at once.
struct Nesting<'r> {
    inner: Regex<'r>,
    input: Vec<u32>,
    outcome: Option<Result<bool, RegexError>>,
    budget: Budget,
}

impl RegexResources for Nesting<'_> {
    fn reserve(&mut self, bytes: usize) -> bool {
        self.budget.reserve(bytes)
    }

    fn release(&mut self, bytes: usize) {
        self.budget.release(bytes);
    }

    fn cancel_requested(&mut self) -> bool {
        false
    }

    fn stack_too_deep(&mut self, depth: usize) -> bool {
        if self.outcome.is_none() && depth >= MAX_STACK_DEPTH / 10 * 9 {
            let mut spans = vec![MatchSpan::UNSET; self.inner.subexpression_count() + 1];
            self.outcome = Some(self.inner.exec(&self.input, 0, &mut spans));
        }
        false
    }
}

#[test]
fn a_match_inside_a_deep_operation_shares_the_frame_budget() {
    let mut inner_budget = Budget::unlimited();
    // Matching dissects the 280 groups recursively; alone it fits easily.
    let inner = Regex::compile(
        &nested("(", 280),
        REG_ADVANCED,
        RegexCollation::C,
        &mut inner_budget,
    )
    .unwrap();
    let mut hooks = Nesting {
        inner,
        input: code_points("a"),
        outcome: None,
        budget: Budget::unlimited(),
    };
    // The outer compilation peaks below the budget, but not with the match on top of it.
    let outer = Regex::compile(
        &nested("(?:", 360),
        REG_ADVANCED,
        RegexCollation::C,
        &mut hooks,
    );
    assert!(outer.is_ok(), "{:?}", outer.as_ref().err());
    drop(outer);
    assert_eq!(hooks.outcome, Some(Err(RegexError::Code(REG_ETOOBIG))));
    let mut spans = vec![MatchSpan::UNSET; 281];
    assert_eq!(hooks.inner.exec(&code_points("a"), 0, &mut spans), Ok(true));
    drop(hooks);
    assert_eq!(inner_budget.live, 0);
    assert_ordinary_operations_work();
}
