//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::CommandPlan;

mod fixtures;
mod metadata;

fn assert_copy(source: &UnifiedPlan) {
    let expected_wire = serde_json::to_value(source).unwrap();
    let expected_transient = format!("{source:?}");
    let budget = MemoryBudget::new(16 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut copied = None;
    let allocation = allocation_counter::measure(|| {
        copied = Some(source.clone_with_control(&control).unwrap());
    });
    let copied = copied.unwrap();
    assert_eq!(copied.reserved_bytes(), budget.used());
    assert_eq!(
        allocation.bytes_current,
        copied.reserved_bytes() as i64,
        "{allocation:?}"
    );
    assert_eq!(serde_json::to_value(&*copied).unwrap(), expected_wire);
    assert_eq!(format!("{:?}", *copied), expected_transient);
    drop(copied);
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_syntax_copy_preserves_utility_and_nested_statement_semantics() {
    let mut compilation_errors = Vec::new();
    for sql in fixtures::STATEMENTS {
        match crate::compile(sql) {
            Ok(mut statements) => assert_copy(&UnifiedPlan::lower(statements.remove(0))),
            Err(error) => compilation_errors.push(format!("{sql}: {error}")),
        }
    }
    assert!(compilation_errors.is_empty(), "{compilation_errors:#?}");
    // Native resolution and host preparation can supply these variants without a separate SQL parser spelling.
    for command in [
        CommandPlan::CreateForeignTable(crate::ast::CreateForeignTable {
            name: "resolved_foreign".into(),
            server_name: "server".into(),
            columns: Vec::new(),
            checks: Vec::new(),
            not_null_declarations: Some(Vec::new()),
            options: vec![("source".into(), "original_source".into())],
            if_not_exists: true,
        }),
        CommandPlan::Empty,
    ] {
        assert_copy(&UnifiedPlan::Command(Box::new(command)));
    }
}

fn large_utility() -> UnifiedPlan {
    UnifiedPlan::Command(Box::new(CommandPlan::Explain {
        options: crate::explain::ExplainOptions::default(),
        body: Box::new(UnifiedPlan::Command(Box::new(CommandPlan::SetVariable {
            name: "application_name".into(),
            value: "x".repeat(4 * 1024 * 1024),
            local: false,
            is_default: false,
        }))),
        unbound_parameters: None,
    }))
}

#[test]
fn complete_syntax_copy_admits_payloads_before_allocation_and_releases_partial_work() {
    let source = large_utility();
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let copied = source
        .clone_with_control(&ProductionControl::new(&budget, &token, &token))
        .unwrap();
    let required = copied.reserved_bytes();
    assert!(required > 4 * 1024 * 1024);
    drop(copied);
    for allowance in [0, 1, 1024, required / 2, required - 1] {
        let child = budget.child(allowance);
        let allocation = allocation_counter::measure(|| {
            let error = source
                .clone_with_control(&ProductionControl::new(&child, &token, &token))
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"));
        });
        assert_eq!(allocation.bytes_current, 0, "{allocation:?}");
        assert!(
            allocation.bytes_total < 16 * 1024,
            "{allowance}: {allocation:?}"
        );
        assert_eq!(budget.used(), 0);
    }
    let sibling = budget.reserve(budget.available() - required + 1).unwrap();
    let child = budget.child(required);
    assert_eq!(
        source
            .clone_with_control(&ProductionControl::new(&child, &token, &token))
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), sibling.bytes());
    drop(sibling);
    let copied = source
        .clone_with_control(&ProductionControl::new(&child, &token, &token))
        .unwrap();
    assert_eq!(copied.reserved_bytes(), required);
    drop(copied);
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_syntax_copy_honors_both_cancellation_scopes_and_unwind_cleanup() {
    let source = large_utility();
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    // Cancellation deferral consults the runtime's thread handle; initialize that thread-owned allocation outside the copy measurement.
    let _thread = std::thread::current();
    for cancel_original in [false, true] {
        let allocation = allocation_counter::measure(|| {
            // Include token-owned lazy mutex storage and its destruction in the same measurement.
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
            let error = source
                .clone_with_control(&ProductionControl::new(&budget, &original, &invoking))
                .unwrap_err();
            assert!(matches!(error.primary(), crate::SQLError::Cancelled(_)));
        });
        assert!(allocation.bytes_total < 1024, "{allocation:?}");
        assert_eq!(allocation.bytes_current, 0, "{allocation:?}");
        assert_eq!(budget.used(), 0);
    }
    let token = CancellationToken::new();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _copy = source
            .clone_with_control(&ProductionControl::new(&budget, &token, &token))
            .unwrap();
        panic!("consumer unwind");
    }))
    .is_err());
    assert_eq!(budget.used(), 0);
}
