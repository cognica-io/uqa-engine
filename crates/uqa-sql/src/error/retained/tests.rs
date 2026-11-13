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
    assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(memory.used(), 0);
    let ordinary = ProductionControl::uncontrolled();
    let error = SQLError::admit_routine_message(
        "42704",
        ordinary.copy_text("missing role").unwrap(),
        &ordinary,
    )
    .unwrap();
    assert!(matches!(error, SQLError::Routine { .. }));
}
