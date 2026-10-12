//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn collation_name_selection_admits_scratch_but_reuses_selected_identity() {
    let token = CancellationToken::new();
    let empty = MemoryBudget::new(0);
    let control = ProductionControl::new(&empty, &token, &token);
    assert_eq!(
        select_with_control(
            "unavailable",
            Some(950),
            Some(&ColumnType::Text),
            None,
            &control
        )
        .unwrap(),
        950
    );
    assert_eq!(empty.peak(), 0);
    assert_eq!(
        select_with_control("\"C\"", None, None, None, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    let budget = MemoryBudget::new(1024);
    let control = ProductionControl::new(&budget, &token, &token);
    for (name, oid) in [("\"C\"", 950), ("pg_catalog.\"POSIX\"", 951)] {
        assert_eq!(
            select_with_control(name, None, None, None, &control).unwrap(),
            oid
        );
        assert_eq!(budget.used(), 0);
    }
    assert!(budget.peak() > 0);
}

#[test]
fn collation_selection_preserves_type_error_order_and_releases_diagnostics() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let error = select_with_control(
        "missing.\"C\"",
        None,
        Some(&ColumnType::Integer),
        None,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(
        error.to_string(),
        "collations are not supported by type integer"
    );
    assert_eq!(budget.used(), 0);
    let error = select_with_control("missing.\"C\"", None, None, None, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("3F000"));
    assert_eq!(error.to_string(), "schema \"missing\" does not exist");
    assert_eq!(budget.used(), 0);
    let domain = ColumnType::Domain {
        schema: "Hidden".into(),
        name: "number".into(),
        oid: 40000,
        array_oid: None,
        base: Box::new(ColumnType::Integer),
    };
    let error = select_with_control("\"C\"", None, Some(&domain), None, &control).unwrap_err();
    assert_eq!(
        error.to_string(),
        "collations are not supported by type \"Hidden\".number"
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn collation_selection_never_hides_resource_failure_as_a_name_error() {
    let budget = MemoryBudget::new(128);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let error =
        select_with_control(&"namespace.".repeat(512), None, None, None, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    assert!(budget.peak() <= budget.limit());
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        for selected in [None, Some(950)] {
            let error = select_with_control("\"C\"", selected, None, None, &control).unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
        }
        assert_eq!(budget.used(), 0);
    }
}
