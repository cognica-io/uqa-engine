//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn admitted_routine_diagnostic_preserves_primary_state_context_and_original_owner() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let text = "role 한🙂 does not exist".repeat(1024);
    let error =
        SQLError::admit_routine_message("42704", control.copy_text(&text).unwrap(), &control)
            .unwrap();
    assert_eq!(error.to_string(), text);
    assert_eq!(error.sqlstate(), Some("42704"));
    assert!(matches!(error.primary(), SQLError::Routine { .. }));
    let used = memory.used();
    assert!(used >= text.len());
    let cloned = error
        .clone()
        .with_context("restore catalog")
        .at_position(Some(7));
    assert_eq!(memory.used(), used);
    drop(error);
    assert_eq!(memory.used(), used);
    assert_eq!(cloned.to_string(), text);
    assert_eq!(cloned.sqlstate(), Some("42704"));
    assert_eq!(cloned.context(), Some("restore catalog"));
    assert_eq!(cloned.position(), Some(7));
    let without_position = cloned.without_position();
    assert_eq!(without_position.position(), None);
    assert!(matches!(
        without_position.primary(),
        SQLError::Routine { .. }
    ));
    drop(without_position);
    assert_eq!(memory.used(), 0);
}

#[test]
fn admitted_routine_diagnostic_releases_payloads_when_header_admission_fails() {
    let memory = MemoryBudget::new(64);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let message = control.copy_text("missing role").unwrap();
    let result = SQLError::admit_routine_message("42704", message, &control);
    let error = result
        .unwrap_err()
        .with_context("restore routine")
        .at_position(Some(7));
    let diagnostic = error.to_string();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(memory.used(), 0);
    let held = memory.reserve(memory.available()).unwrap();
    let error = error.into_retained_catalog_message(&control).unwrap();
    let Some(uqa_core::ValueRetentionError::Memory(uqa_core::memory::MemoryError::Limit {
        required,
        limit,
    })) = error.native_resource_error()
    else {
        panic!("catalog retention discarded the original memory error");
    };
    assert_eq!(limit, 64);
    assert!(required > limit);
    assert_eq!(error.to_string(), diagnostic);
    assert_eq!(error.position(), Some(7));
    assert_eq!(error.context(), Some("restore routine"));
    drop(held);
    assert_eq!(memory.used(), 0);
    let ordinary = ProductionControl::uncontrolled();
    let error = SQLError::admit_routine_message(
        "42704",
        ordinary.copy_text("missing role").unwrap(),
        &ordinary,
    )
    .unwrap();
    assert!(matches!(error, SQLError::Routine { .. }));
    let raised = SQLError::Routine {
        sqlstate: "53200".into(),
        message: diagnostic,
    };
    assert!(raised.native_resource_error().is_none());
}
