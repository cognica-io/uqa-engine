//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn type_diagnostics_reject_large_borrowed_names_before_message_growth() {
    let names = vec!["catalog".into(), "schema".into(), "x".repeat(1 << 22)];
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let allocations = allocation_counter::measure(|| {
        assert_eq!(cross_database(&names, &control).sqlstate(), Some("53200"));
    });
    assert!(allocations.bytes_total < 16 * 1024, "{allocations:?}");
    assert_eq!(memory.used(), 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        assert_eq!(cross_database(&names, &control).sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn type_diagnostics_preserve_original_names_states_and_order() {
    let names = vec!["a.b".into(), "c\"d".into(), "e".into()];
    let control = ProductionControl::uncontrolled();
    assert_eq!(
        cross_database(&names, &control).to_string(),
        SQLError::Unsupported("cross-database references are not implemented: a.b.c\"d.e".into())
            .to_string(),
    );
    let error = routine(
        "42704",
        format_args!("type \"{}\" does not exist", QualifiedNames(&names[..2])),
        &control,
    );
    assert_eq!(error.sqlstate(), Some("42704"));
    assert_eq!(
        error.to_string(),
        SQLError::Routine {
            sqlstate: "42704".into(),
            message: "type \"a.b.c\"d\" does not exist".into(),
        }
        .to_string(),
    );
}
