//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::{RoutineBody, SQLUserFunction};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn routine(count: usize, standard: bool, last_default: &str) -> SQLRoutine {
    let params: Vec<_> = (1..=count)
        .map(|index| {
            if index == count {
                format!("p{index} text DEFAULT '{last_default}'")
            } else {
                format!("p{index} text")
            }
        })
        .collect();
    let body = if standard {
        "BEGIN ATOMIC SELECT 1; SELECT 2; END"
    } else {
        "AS 'SELECT 2'"
    };
    let sql = format!(
        "CREATE FUNCTION public.saved({}) RETURNS integer LANGUAGE SQL {body}",
        params.join(", ")
    );
    let Statement::CreateFunction(definition) = crate::compile(&sql).unwrap().remove(0) else {
        panic!("expected routine declaration");
    };
    SQLUserFunction::new(*definition, RoutineBody::Source).shared()
}

#[test]
fn stored_routine_parameters_and_statements_are_copied_in_separate_windows() {
    for (count, standard) in [(0, false), (1, true), (64, false), (64, true)] {
        let source = routine(count, standard, "saved value");
        let budget = MemoryBudget::new(4 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut copying = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
        assert!(copying.declaration().params.is_empty());
        let statements = if standard { 2 } else { 0 };
        let fields = count + statements;
        for step in 1..=fields.max(1) {
            let finished = copying.copy_next_field(&control).unwrap();
            assert_eq!(finished, step == fields.max(1));
            assert_eq!(copying.declaration().params.len(), count.min(step));
            if let FunctionBody::Statements(body) = &copying.declaration().body {
                assert_eq!(body.len(), step.saturating_sub(count));
            }
        }
        let ordinary_budget = MemoryBudget::new(4 << 20);
        let ordinary = ProductionControl::new(&ordinary_budget, &token, &token);
        let (expected, expected_children) =
            capture_stored_with_footprint(&source.def, &ordinary).unwrap();
        let (actual, children) = copying.into_definition(&control).unwrap();
        assert_eq!(
            serde_json::to_value(&*actual).unwrap(),
            serde_json::to_value(&*expected).unwrap()
        );
        assert_eq!(children, expected_children);
        assert_eq!(actual.reserved_bytes(), expected.reserved_bytes());
        assert_eq!(actual.reserved_bytes(), budget.used());
        drop((actual, expected));
        assert_eq!(budget.used(), 0);
        assert_eq!(ordinary_budget.used(), 0);
    }
}

#[test]
fn stored_routine_parameters_release_partial_copies_after_denial_or_cancellation() {
    let source = routine(64, false, &"x".repeat(128 << 10));
    let budget = MemoryBudget::new(64 << 10);
    for cancel in [true, false] {
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut copying = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
        for _ in 0..63 {
            assert!(!copying.copy_next_field(&control).unwrap());
        }
        let before = budget.used();
        if cancel {
            token.cancel();
        }
        let error = copying.copy_next_field(&control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if cancel { "57014" } else { "53200" })
        );
        assert_eq!(copying.declaration().params.len(), 63);
        assert_eq!(budget.used(), before);
        assert!(copying.into_definition(&control).is_err());
        assert_eq!(budget.used(), 0);
    }
}
