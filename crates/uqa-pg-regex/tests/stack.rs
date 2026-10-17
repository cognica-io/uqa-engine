//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stack headroom for the engine's recursion.
//!
//! The engine recurses on the stack and may use up to `MAX_STACK_DEPTH`, `PostgreSQL`'s default `max_stack_depth` of 2 MB. These tests run operations that need more engine stack than their thread has left, which complete only when the wrapper runs them on a separately allocated stack, and operations beyond the budget, which fail as `PostgreSQL`'s do. A thread may get more stack than it requests, for example a cached larger stack from the C library, so the tests use up their thread's stack down to a known remainder, measured as the wrapper measures it. WebAssembly runs the engine on its own call stack, which can be neither measured nor switched, so the tests run on native targets only.
#![cfg(not(target_family = "wasm"))]

use std::hint::black_box;
use std::ops::RangeInclusive;
use std::panic;
use std::ptr;
use std::thread;

use uqa_pg_regex::{
    MatchSpan, Regex, RegexCollation, MAX_STACK_DEPTH, REG_ADVANCED, REG_ETOOBIG, STACK_DEPTH_SLOP,
};

use crate::support::{code_points, Budget, SharedBudget};

/// `count` groups opened with `opening` around `a`; the parser recurses once per group.
fn nested(opening: &str, count: usize) -> String {
    format!("{}a{}", opening.repeat(count), ")".repeat(count))
}

/// The address of a local of the calling frame.
fn frame_address() -> usize {
    let marker = 0u8;
    black_box(ptr::addr_of!(marker)) as usize
}

/// Run `run` on a new thread with `size` bytes of stack, passing the address of the thread's first frame.
fn on_thread<T: Send + 'static>(size: usize, run: impl FnOnce(usize) -> T + Send + 'static) -> T {
    let worker = thread::Builder::new()
        .stack_size(size)
        .spawn(move || run(frame_address()))
        .expect("the test thread starts");
    worker
        .join()
        .unwrap_or_else(|payload| panic::resume_unwind(payload))
}

/// Use the current thread's stack until at most `left` bytes remain, then run `run` with the range from the end of the thread's stack to its first frame at `top`.
fn with_stack_left<T>(
    top: usize,
    left: usize,
    run: &mut dyn FnMut(RangeInclusive<usize>) -> T,
) -> T {
    let remaining = stacker::remaining_stack().expect("test threads have a known stack limit");
    if remaining <= left {
        return run(frame_address() - remaining..=top);
    }
    let padding = black_box([0u8; 4096]);
    let result = with_stack_left(top, left, run);
    black_box(&padding);
    result
}

/// Whether every frame the stack-depth hook saw lies outside `stack`.
fn ran_outside(stack: &RangeInclusive<usize>, budget: &Budget) -> bool {
    let (lowest, highest) = budget.frames.expect("the engine checked its depth");
    !stack.contains(&lowest) && !stack.contains(&highest)
}

/// Compile `pattern` with a hook that refuses beyond `MAX_STACK_DEPTH`, the limit callers apply, and return the number of reservations it made.
fn reservations_of_compiling(pattern: &str) -> usize {
    let mut budget = Budget {
        stack_limit: Some(MAX_STACK_DEPTH),
        ..Budget::default()
    };
    drop(Regex::compile(pattern, REG_ADVANCED, RegexCollation::C, &mut budget).unwrap());
    assert_eq!(budget.live, 0);
    budget.reservations
}

#[test]
fn deep_operations_on_a_small_thread_run_on_a_separate_stack() {
    const LEFT: usize = 128 * 1024;
    on_thread(512 * 1024, |top| {
        with_stack_left(top, LEFT, &mut |stack| {
            let shared = SharedBudget::default();
            shared.0.borrow_mut().stack_limit = Some(MAX_STACK_DEPTH);
            let mut hooks = shared.clone();
            // PostgreSQL 18.4 accepts 5000 nested groups; parsing them needs more than a megabyte of engine stack.
            let mut regex = Regex::compile(
                &nested("(?:", 5000),
                REG_ADVANCED,
                RegexCollation::C,
                &mut hooks,
            )
            .unwrap();
            let deepest = shared.0.borrow().deepest;
            assert!(deepest > 1024 * 1024, "engine stack of {deepest} bytes");
            assert!(ran_outside(&stack, &shared.0.borrow()));
            let mut spans = [MatchSpan::UNSET; 1];
            assert!(regex.exec(&code_points("xay"), 0, &mut spans).unwrap());
            assert_eq!(spans[0].range(), Some(1..2));
            drop(regex);
            assert_eq!(shared.0.borrow().live, 0);
        });
    });
}

#[test]
fn an_execution_with_little_stack_left_runs_on_a_separate_stack() {
    const LEFT: usize = 96 * 1024;
    on_thread(1024 * 1024, |top| {
        let shared = SharedBudget::default();
        let mut hooks = shared.clone();
        // Matching dissects 1000 nested capturing groups recursively.
        let mut regex = Regex::compile(
            &nested("(", 1000),
            REG_ADVANCED,
            RegexCollation::C,
            &mut hooks,
        )
        .unwrap();
        let mut spans = vec![MatchSpan::UNSET; 1001];
        {
            let mut budget = shared.0.borrow_mut();
            budget.frames = None;
            budget.deepest = 0;
        }
        let input = code_points("a");
        let (matched, stack) = with_stack_left(top, LEFT, &mut |stack| {
            (regex.exec(&input, 0, &mut spans).unwrap(), stack)
        });
        assert!(matched);
        assert!(spans.iter().all(|span| span.range() == Some(0..1)));
        let budget = shared.0.borrow();
        assert!(
            budget.deepest > LEFT,
            "execution engine stack of {} bytes",
            budget.deepest
        );
        assert!(ran_outside(&stack, &budget));
    });
}

#[test]
fn an_operation_that_outgrows_its_thread_stack_starts_again_on_a_separate_stack() {
    const THREAD_STACK: usize = 8 * 1024 * 1024;
    let pattern = nested("(?:", 5000);
    let copy = pattern.clone();
    // With headroom to spare the compilation runs once on the thread's own stack.
    let once = on_thread(THREAD_STACK, move |_| reservations_of_compiling(&copy));
    // With 1 MiB left the first attempt runs on the thread's stack until the pattern outgrows it; its reservations come back before the second attempt.
    let restarted = on_thread(THREAD_STACK, move |top| {
        with_stack_left(top, 1024 * 1024, &mut |_| {
            reservations_of_compiling(&pattern)
        })
    });
    assert!(
        restarted > once,
        "{restarted} reservations with a restart, {once} without"
    );
}

#[test]
fn operations_beyond_the_stack_budget_report_regular_expression_too_complex() {
    assert_eq!(MAX_STACK_DEPTH, 2048 * 1024);
    assert_eq!(STACK_DEPTH_SLOP, 512 * 1024);
    // PostgreSQL 18.4 rejects 10000 nested groups at its default max_stack_depth. The hook never refuses, so the engine stops at the budget itself.
    for opening in ["(", "(?:"] {
        let mut budget = Budget::unlimited();
        let error = Regex::compile(
            &nested(opening, 10000),
            REG_ADVANCED,
            RegexCollation::C,
            &mut budget,
        )
        .err()
        .unwrap_or_else(|| panic!("10000 nested {opening} groups compiled"));
        assert_eq!(error.code(), Some(REG_ETOOBIG));
        assert_eq!(error.message(), "regular expression is too complex");
        assert!(budget.deepest <= MAX_STACK_DEPTH, "{}", budget.deepest);
        assert_eq!(budget.live, 0);
    }
}
