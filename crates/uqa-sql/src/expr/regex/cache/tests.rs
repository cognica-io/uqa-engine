//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::regex::engine::{code_points, RegexCall};
use uqa_core::{memory::ProductionControl, CancellationToken};
use uqa_pg_regex::{REG_ADVANCED, REG_ICASE};

thread_local! {
    static COMPILES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(in crate::expr::regex) fn compiled() {
    COMPILES.set(COMPILES.get() + 1);
}

fn predicate(
    pattern: &str,
    flags: CompileFlags,
    collation: u32,
    text: &str,
    control: &ProductionControl<'_>,
) -> crate::error::Result<bool> {
    let call = RegexCall::new(control);
    let mut allowance = call.allowance();
    let mut regex = call.compile(pattern, flags, collation, &mut allowance)?;
    let input = code_points(text, control)?;
    call.execute(&mut regex, &input, 0, &mut [])
}

#[test]
fn repeated_patterns_compile_once_and_release_the_complete_retained_owner() {
    let budget = MemoryBudget::new(1 << 20);
    let cache = Arc::new(RegexCache::new(budget.clone()));
    let scope = RegexCacheScope::enter(Some(cache.clone()));
    let before = COMPILES.get();
    for _ in 0..1000 {
        assert!(predicate(
            "^(a|ab)+$",
            REG_ADVANCED,
            950,
            "abab",
            &ProductionControl::uncontrolled()
        )
        .unwrap());
    }
    assert_eq!(COMPILES.get() - before, 1);
    assert!(budget.used() > 0);
    drop(scope);
    drop(cache);
    assert_eq!(budget.used(), 0);
}

#[test]
fn keys_distinguish_pattern_flags_and_collation_and_keep_postgresql_results() {
    let cache = Arc::new(RegexCache::new(MemoryBudget::new(1 << 20)));
    let _scope = RegexCacheScope::enter(Some(cache));
    let before = COMPILES.get();
    for _ in 0..3 {
        for (pattern, flags, collation, text, expected) in [
            ("é", REG_ADVANCED, 811, "É", false),
            ("é", REG_ADVANCED | REG_ICASE, 811, "É", true),
            ("é", REG_ADVANCED | REG_ICASE, 950, "É", false),
            ("^[[:alpha:]]$", REG_ADVANCED, 811, "é", true),
            ("^[[:alpha:]]$", REG_ADVANCED, 950, "é", false),
        ] {
            assert_eq!(
                predicate(
                    pattern,
                    flags,
                    collation,
                    text,
                    &ProductionControl::uncontrolled()
                )
                .unwrap(),
                expected
            );
        }
    }
    assert_eq!(COMPILES.get() - before, 5);
}

#[test]
fn the_thirty_two_entry_limit_evicts_the_least_recently_used_pattern() {
    let budget = MemoryBudget::new(4 << 20);
    let cache = Arc::new(RegexCache::new(budget.clone()));
    let _scope = RegexCacheScope::enter(Some(cache.clone()));
    let control = ProductionControl::uncontrolled();
    let before = COMPILES.get();
    for index in 0..32 {
        predicate(&format!("p{index}"), REG_ADVANCED, 950, "", &control).unwrap();
    }
    predicate("p0", REG_ADVANCED, 950, "", &control).unwrap();
    predicate("p32", REG_ADVANCED, 950, "", &control).unwrap();
    predicate("p0", REG_ADVANCED, 950, "", &control).unwrap();
    assert_eq!(COMPILES.get() - before, 33);
    predicate("p1", REG_ADVANCED, 950, "", &control).unwrap();
    assert_eq!(COMPILES.get() - before, 34);
    assert_eq!(cache.entries.lock().unwrap().values.len(), 32);
    cache.clear();
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_compilation_does_not_evict_a_full_cache_without_memory_pressure() {
    let budget = MemoryBudget::new(4 << 20);
    let cache = Arc::new(RegexCache::new(budget.clone()));
    let _scope = RegexCacheScope::enter(Some(cache.clone()));
    let control = ProductionControl::uncontrolled();
    for index in 0..32 {
        predicate(&format!("p{index}"), REG_ADVANCED, 950, "", &control).unwrap();
    }
    let retained = budget.used();
    let error = predicate("(", REG_ADVANCED, 950, "", &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("2201B"));
    assert_eq!(budget.used(), retained);
    assert_eq!(cache.entries.lock().unwrap().values.len(), 32);
    let before = COMPILES.get();
    for index in 0..32 {
        predicate(&format!("p{index}"), REG_ADVANCED, 950, "", &control).unwrap();
    }
    assert_eq!(COMPILES.get(), before);
    cache.clear();
    assert_eq!(budget.used(), 0);
}

#[test]
fn hits_observe_each_current_cancellation_and_scratch_allowance() {
    let retained = MemoryBudget::new(1 << 20);
    let cache = Arc::new(RegexCache::new(retained.clone()));
    let _scope = RegexCacheScope::enter(Some(cache.clone()));
    predicate(
        r"(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)(l)(m)(n)(o)(p)(q)(r)(s)(t)(u)(?<=u)v\1",
        REG_ADVANCED,
        950,
        "zzabcdefghijklmnopqrstuva",
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    let kept = retained.used();
    let before = COMPILES.get();
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = predicate(
            r"(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)(l)(m)(n)(o)(p)(q)(r)(s)(t)(u)(?<=u)v\1",
            REG_ADVANCED,
            950,
            "zzabcdefghijklmnopqrstuva",
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
        assert_eq!(retained.used(), kept);
    }
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let call = RegexCall::new(&control);
    let mut allowance = call.allowance();
    let mut regex = call
        .compile(
            r"(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)(l)(m)(n)(o)(p)(q)(r)(s)(t)(u)(?<=u)v\1",
            REG_ADVANCED,
            950,
            &mut allowance,
        )
        .unwrap();
    assert_eq!(
        call.execute(
            &mut regex,
            &"zzabcdefghijklmnopqrstuva"
                .chars()
                .map(u32::from)
                .collect::<Vec<_>>(),
            0,
            &mut []
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    drop(regex);
    assert_eq!(COMPILES.get(), before);
    assert_eq!(retained.used(), kept);
    cache.clear();
    assert_eq!(retained.used(), 0);
}

#[test]
fn cache_scopes_restore_nested_sessions_and_move_compiled_state_to_a_worker() {
    let outer = Arc::new(RegexCache::new(MemoryBudget::new(1 << 20)));
    let inner = Arc::new(RegexCache::new(MemoryBudget::new(1 << 20)));
    let scope = RegexCacheScope::enter(Some(outer.clone()));
    predicate(
        "(ab)+",
        REG_ADVANCED,
        950,
        "abab",
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    {
        let _nested = RegexCacheScope::enter(Some(inner.clone()));
        assert!(Arc::ptr_eq(&current_regex_cache().unwrap(), &inner));
    }
    assert!(Arc::ptr_eq(&current_regex_cache().unwrap(), &outer));
    let captured = current_regex_cache();
    std::thread::spawn(move || {
        let _scope = RegexCacheScope::enter(captured);
        let before = COMPILES.get();
        assert!(predicate(
            "(ab)+",
            REG_ADVANCED,
            950,
            "abab",
            &ProductionControl::uncontrolled()
        )
        .unwrap());
        assert_eq!(
            COMPILES.get(),
            before,
            "worker reuses the compiled allocation"
        );
    })
    .join()
    .unwrap();
    drop(scope);
    assert!(current_regex_cache().is_none());
}

#[test]
fn failed_compilations_keep_errors_and_release_partial_contexts() {
    let budget = MemoryBudget::new(1 << 20);
    let cache = Arc::new(RegexCache::new(budget.clone()));
    let _scope = RegexCacheScope::enter(Some(cache.clone()));
    let before = COMPILES.get();
    for _ in 0..2 {
        let invalid_collation =
            predicate("(", REG_ADVANCED, 0, "", &ProductionControl::uncontrolled()).unwrap_err();
        assert_eq!(invalid_collation.sqlstate(), Some("42P22"));
        let invalid = predicate(
            "(",
            REG_ADVANCED,
            950,
            "",
            &ProductionControl::uncontrolled(),
        )
        .unwrap_err();
        assert_eq!(invalid.sqlstate(), Some("2201B"));
        assert_eq!(
            invalid.to_string(),
            "invalid regular expression: parentheses () not balanced"
        );
    }
    assert_eq!(COMPILES.get(), before);
    assert!(cache.entries.lock().unwrap().values.is_empty());
    cache.clear();
    assert_eq!(budget.used(), 0);
}

#[test]
fn cleared_active_patterns_cannot_repopulate_a_new_cache_generation() {
    let budget = MemoryBudget::new(1 << 20);
    let cache = Arc::new(RegexCache::new(budget.clone()));
    let _scope = RegexCacheScope::enter(Some(cache.clone()));
    let control = ProductionControl::uncontrolled();
    let call = RegexCall::new(&control);
    let mut allowance = call.allowance();
    let active = call
        .compile("old", REG_ADVANCED, 950, &mut allowance)
        .unwrap();
    cache.clear();
    assert!(budget.used() > 0, "active compiled bytes are still owned");
    predicate("new", REG_ADVANCED, 950, "new", &control).unwrap();
    drop(active);
    let entries = cache.entries.lock().unwrap();
    assert_eq!(entries.values.len(), 1);
    assert_eq!(&*entries.values[0].pattern, "new");
    drop(entries);
    cache.clear();
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_byte_pressure_reclaims_idle_patterns_before_refusing_compilation() {
    let probe = MemoryBudget::new(1 << 20);
    {
        let cache = Arc::new(RegexCache::new(probe.clone()));
        let _scope = RegexCacheScope::enter(Some(cache.clone()));
        // Different literal digits can produce different compiled layouts. Use
        // the largest isolated peak of the complete fixed workload, not one key.
        for index in 0..64 {
            let pattern = format!("needle{index:02}");
            predicate(
                &pattern,
                REG_ADVANCED,
                950,
                &pattern,
                &ProductionControl::uncontrolled(),
            )
            .unwrap();
            cache.clear();
        }
    }
    let budget = MemoryBudget::new(probe.peak());
    let cache = Arc::new(RegexCache::new(budget.clone()));
    let _scope = RegexCacheScope::enter(Some(cache.clone()));
    for index in 0..64 {
        let pattern = format!("needle{index:02}");
        assert!(predicate(
            &pattern,
            REG_ADVANCED,
            950,
            &pattern,
            &ProductionControl::uncontrolled()
        )
        .unwrap());
    }
    assert!(cache.entries.lock().unwrap().values.len() < CAPACITY);
    assert!(budget.peak() <= budget.limit());
    cache.clear();
    assert_eq!(budget.used(), 0);
}

#[test]
fn simultaneous_invocations_do_not_hold_the_cache_mutex_during_matching() {
    let budget = MemoryBudget::new(1 << 20);
    let cache = Arc::new(RegexCache::new(budget.clone()));
    let _scope = RegexCacheScope::enter(Some(cache.clone()));
    let control = ProductionControl::uncontrolled();
    let call = RegexCall::new(&control);
    let mut allowance = call.allowance();
    let mut active = call
        .compile("a+", REG_ADVANCED, 950, &mut allowance)
        .unwrap();
    let captured = current_regex_cache();
    std::thread::spawn(move || {
        let _scope = RegexCacheScope::enter(captured);
        assert!(predicate(
            "a+",
            REG_ADVANCED,
            950,
            "aaa",
            &ProductionControl::uncontrolled()
        )
        .unwrap());
    })
    .join()
    .unwrap();
    assert!(call.execute(&mut active, &[97, 97], 0, &mut []).unwrap());
    drop(active);
    assert_eq!(cache.entries.lock().unwrap().values.len(), 1);
    cache.clear();
    assert_eq!(budget.used(), 0);
}

#[test]
fn cache_admission_failures_return_every_partial_charge() {
    // These are memory limits, not timed trials. Refuse the slot, key and
    // compilation allocations in turn until the complete pattern can be held.
    for limit in (0..=262_144).step_by(4096) {
        let budget = MemoryBudget::new(limit);
        let cache = Arc::new(RegexCache::new(budget.clone()));
        let scope = RegexCacheScope::enter(Some(cache.clone()));
        let result = predicate(
            "(a|b)*c{1,20}",
            REG_ADVANCED,
            950,
            "abc",
            &ProductionControl::uncontrolled(),
        );
        if let Err(error) = &result {
            assert_eq!(error.sqlstate(), Some("53200"));
        }
        drop(scope);
        drop(cache);
        assert_eq!(budget.used(), 0, "limit {limit}");
        if result.is_ok() {
            return;
        }
    }
    panic!("the fixed fixture must fit the final allowance");
}
