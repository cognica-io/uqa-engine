//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::plan::{CtePlan, UnifiedPlan};

fn plans(sql: &str) -> Vec<CtePlan> {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    query.ctes
}

fn scheduled(plan: &CtePlan, deferred: bool) -> ScheduledCte<'_> {
    ScheduledCte { plan, deferred }
}

#[test]
fn postponed_cte_readers_keep_plan_owners_after_once_only_schedule_transfer() {
    let input = plans(&format!(
        "WITH first AS (SELECT '{}' AS payload), second AS (SELECT 2) SELECT 1",
        "x".repeat(4 << 20)
    ));
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let commands = StatementCommands::with_control(&control).unwrap();
    assert_eq!(budget.used(), size_of::<StatementCommands>());
    commands
        .begin_data_modifying_with(
            [scheduled(&input[1], false), scheduled(&input[0], true)],
            &control,
        )
        .unwrap();
    assert!(commands.modifies_with());
    let before = budget.used();
    assert!(before > 4 << 20);
    let other = Arc::clone(&commands);
    let queue = commands.take_scheduled_postponed().unwrap();
    assert!(other.take_scheduled_postponed().is_none());
    assert_eq!(queue[0].0.name, "second");
    assert!(!queue[0].1);
    assert_eq!(queue[1].0.name, "first");
    assert!(queue[1].1);
    let independent = queue[1].0.clone();
    assert_eq!(budget.used(), before);
    drop(commands);
    assert_eq!(budget.used(), before);
    drop(other);
    assert_eq!(budget.used(), before - size_of::<StatementCommands>());
    drop(queue);
    assert!(budget.used() > 4 << 20 && budget.used() < before);
    assert_eq!(independent.name, "first");
    drop(independent);
    assert_eq!(budget.used(), 0);
}

#[test]
fn rejected_postponed_replacement_preserves_prior_schedule_and_releases_partial_work() {
    let input = plans(&format!(
        "WITH small AS (SELECT 1), large AS (SELECT '{}') SELECT 1",
        "x".repeat(4 << 20)
    ));
    let budget = MemoryBudget::new(16 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let commands = StatementCommands::with_control(&control).unwrap();
    commands
        .begin_data_modifying_with([scheduled(&input[0], true)], &control)
        .unwrap();
    let baseline = budget.used();
    let pointer = std::ptr::from_ref(commands.postponed.lock().as_ref().unwrap()[0].0.as_ref());
    let occupied = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocations = allocation_counter::measure(|| {
        result = Some(commands.begin_data_modifying_with(
            [scheduled(&input[0], false), scheduled(&input[1], false)],
            &control,
        ));
    });
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert!(allocations.bytes_total < 256 << 10, "{allocations:?}");
    assert_eq!(budget.used(), baseline + occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        let entries = input.iter().enumerate().map(|(index, plan)| {
            if index == 1 {
                token.cancel();
            }
            scheduled(plan, false)
        });
        assert_eq!(
            commands
                .begin_data_modifying_with(entries, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), baseline);
        token.reset();
    }
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let entries = input.iter().enumerate().map(|(index, plan)| {
            assert_eq!(index, 0, "injected schedule observation unwind");
            scheduled(plan, false)
        });
        commands
            .begin_data_modifying_with(entries, &control)
            .unwrap();
    }));
    assert!(unwind.is_err());
    assert_eq!(budget.used(), baseline);
    let retained = commands.take_scheduled_postponed().unwrap();
    assert_eq!(retained.len(), 1);
    assert!(retained[0].1);
    assert_eq!(std::ptr::from_ref(retained[0].0.as_ref()), pointer);
    drop((commands, retained));
    assert_eq!(budget.used(), 0);
}

#[test]
fn statement_owner_and_empty_schedule_admission_precede_state_publication() {
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let too_small = MemoryBudget::new(size_of::<StatementCommands>() - 1);
    let control = ProductionControl::new(&too_small, &original, &invoking);
    assert_eq!(
        StatementCommands::with_control(&control)
            .err()
            .unwrap()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(too_small.used(), 0);
    let ancestor = MemoryBudget::new(64 << 10);
    let budget = ancestor.child(64 << 10);
    let control = ProductionControl::new(&budget, &original, &invoking);
    let commands = StatementCommands::with_control(&control).unwrap();
    let baseline = budget.used();
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            commands
                .begin_data_modifying_with([], &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert!(!commands.modifies_with());
        assert!(commands.take_scheduled_postponed().is_none());
        assert_eq!(budget.used(), baseline);
        assert_eq!(ancestor.used(), baseline);
        assert_eq!(
            StatementCommands::with_control(&control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        token.reset();
    }
    commands.begin_data_modifying_with([], &control).unwrap();
    assert!(commands.modifies_with());
    assert!(commands.take_scheduled_postponed().unwrap().is_empty());
    assert!(commands.take_scheduled_postponed().is_none());
    drop(commands);
    assert_eq!(ancestor.used(), 0);
}
