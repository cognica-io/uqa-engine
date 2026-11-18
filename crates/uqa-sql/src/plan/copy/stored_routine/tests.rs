//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::{RoutineBody, SQLUserFunction};
use uqa_core::{
    memory::{MemoryBudget, ProductionString},
    CancellationToken,
};

mod header;

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

fn deferred(text: &str, parameter: &str) -> SQLRoutine {
    let sql = format!(
        "CREATE FUNCTION public.saved({parameter}) RETURNS integer LANGUAGE SQL AS $${text}$$"
    );
    let Statement::CreateFunction(mut definition) = crate::compile(&sql).unwrap().remove(0) else {
        panic!("expected routine declaration");
    };
    definition.catalog_revision = Some([37; 16]);
    assert!(definition.body_origin.is_some());
    SQLUserFunction::new(*definition, RoutineBody::Source).shared()
}

fn source_body(definition: &CreateFunction) -> &String {
    let FunctionBody::Source(text) = &definition.body else {
        panic!("expected a deferred source body");
    };
    text
}

#[test]
fn deferred_source_copies_exact_utf8_windows_and_preserves_later_parameter_edits() {
    let prefix = "SELECT p /*";
    let wide = format!(
        "{prefix}{}{}*/",
        " ".repeat(4095 - prefix.len()),
        "🙂자료".repeat(32768)
    );
    for text in ["SELECT p", wide.as_str()] {
        let source = deferred(text, "p integer DEFAULT 1");
        let token = CancellationToken::new();
        let memory = MemoryBudget::new(4 << 20);
        let control = ProductionControl::new(&memory, &token, &token);
        let mut capture = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
        assert!(capture.declaration().body_origin.is_none());
        let first = source_body(capture.declaration());
        assert_eq!(first.as_str(), ProductionString::first_chunk(text));
        if text.len() > 4096 {
            assert_eq!(first.len(), 4095);
        }
        let pointer = first.as_ptr();
        let capacity = first.capacity();
        assert!(capacity >= text.len());
        let header_bytes = memory.used();
        let mut offset = first.len();
        let mut steps = 0;
        while offset < text.len() {
            assert!(!capture.copy_next_field(&control).unwrap());
            let copied = source_body(capture.declaration());
            let chunk = ProductionString::first_chunk(&text[offset..]);
            assert_eq!(copied.len(), offset + chunk.len());
            assert_eq!(&copied[offset..], chunk);
            assert_eq!(copied.as_ptr(), pointer);
            assert_eq!(copied.capacity(), capacity);
            assert_eq!(memory.used(), header_bytes);
            assert!(capture.declaration().params.is_empty());
            offset = copied.len();
            steps += 1;
        }
        assert_eq!(steps == 0, text.len() < 4096);
        assert!(capture.copy_next_field(&control).unwrap());
        assert_eq!(
            capture
                .edit_parameter(0, &control, |param| {
                    param.name = "migrated_parameter".to_owned();
                    Ok(true)
                })
                .unwrap(),
            Some(true)
        );
        let mut reference = source.def.clone();
        reference.params[0].name = "migrated_parameter".to_owned();
        let ordinary_memory = MemoryBudget::new(4 << 20);
        let ordinary = ProductionControl::new(&ordinary_memory, &token, &token);
        let (expected, children) = capture_stored_with_footprint(&reference, &ordinary).unwrap();
        let (captured, footprint) = capture.into_definition(&control).unwrap();
        assert_eq!(
            serde_json::to_value(&*captured).unwrap(),
            serde_json::to_value(&*expected).unwrap()
        );
        assert_eq!(children, footprint);
        assert_eq!(captured.reserved_bytes(), expected.reserved_bytes());
        assert_eq!(source_body(&captured).as_ptr(), pointer);
        assert_eq!(source_body(&captured), text);
        assert!(captured.body_origin.is_none());
        assert_eq!(source.def.params[0].name, "p");
        assert_eq!(source_body(&source.def), text);
        assert!(source.def.body_origin.is_some());
        drop((captured, expected));
        assert_eq!(memory.used(), 0);
        assert_eq!(ordinary_memory.used(), 0);
    }
}

#[test]
fn deferred_source_keeps_the_ordinary_peak_and_releases_every_unpublished_prefix() {
    let source = deferred(&format!("SELECT 1 /*{}*/", "𐐷".repeat(65536)), "");
    let original = CancellationToken::new();
    let ordinary_memory = MemoryBudget::new(2 << 20);
    let ordinary = ProductionControl::new(&ordinary_memory, &original, &original);
    let (expected, children) = capture_stored_with_footprint(&source.def, &ordinary).unwrap();
    let peak = ordinary_memory.peak();
    let memory = MemoryBudget::new(peak);
    let control = ProductionControl::new(&memory, &original, &original);
    let mut copy = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
    let mut calls = 1;
    while !copy.copy_next_field(&control).unwrap() {
        calls += 1;
    }
    let (captured, count) = copy.into_definition(&control).unwrap();
    assert_eq!(source_body(&captured), source_body(&source.def));
    assert_eq!(children, count);
    assert_eq!(captured.reserved_bytes(), expected.reserved_bytes());
    assert_eq!(memory.peak(), peak);
    drop((captured, expected));
    assert_eq!(memory.used(), 0);
    assert_eq!(ordinary_memory.used(), 0);
    let scarce = MemoryBudget::new(peak - 1);
    let control = ProductionControl::new(&scarce, &original, &original);
    let failure = StoredRoutineDefinition::new(source.clone(), &control)
        .err()
        .unwrap();
    assert_eq!(failure.sqlstate(), Some("53200"));
    assert_eq!(scarce.used(), 0);
    for stop in [0, calls / 2, calls - 1] {
        for cancel in [None, Some(false), Some(true)] {
            let session = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &session, &invoking);
            let mut pending = StoredRoutineDefinition::new(source.clone(), &control).unwrap();
            for _ in 0..stop {
                assert!(!pending.copy_next_field(&control).unwrap());
            }
            if let Some(caller) = cancel {
                if caller {
                    invoking.cancel();
                } else {
                    session.cancel();
                }
                assert_eq!(
                    pending.copy_next_field(&control).unwrap_err().sqlstate(),
                    Some("57014")
                );
            }
            assert!(pending.into_definition(&control).is_err());
            assert_eq!(memory.used(), 0);
        }
    }
}
