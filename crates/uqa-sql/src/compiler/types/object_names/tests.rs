//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_regobject_names_preserve_postgresql_scanner_rules() {
    let cases = [
        (
            "  Select . \"Mixed.Name\" . a-b  ",
            Some(vec!["select", "Mixed.Name", "a-b"]),
        ),
        ("\"a\"\"b\"", Some(vec!["a\"b"])),
        ("\"\"", Some(vec![""])),
        ("a b", None),
        ("a.", None),
        ("a..b", None),
        ("\"unclosed", None),
        (" \t\r\n", None),
    ];
    for (input, expected) in cases {
        let budget = MemoryBudget::new(1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let actual = parse_regobject_name_with_control(input, &control).unwrap();
        assert_eq!(
            actual
                .as_ref()
                .map(|names| names.iter().map(String::as_str).collect::<Vec<_>>()),
            expected,
            "{input}"
        );
        assert_eq!(
            budget.used(),
            actual.as_ref().map_or(0, Produced::reserved_bytes)
        );
        drop(actual);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn regobject_identifier_clipping_never_retains_the_discarded_suffix() {
    let cases = [
        ("A".repeat(32768), "a".repeat(63)),
        (
            format!("{}é{}", "A".repeat(62), "Z".repeat(32768)),
            "a".repeat(62),
        ),
        (format!("\"{}é\"\"suffix\"", "A".repeat(62)), "A".repeat(62)),
        (
            format!("\"{}\"\"suffix\"", "A".repeat(62)),
            format!("{}\"", "A".repeat(62)),
        ),
    ];
    for (input, expected) in cases {
        let budget = MemoryBudget::new(256);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let names = parse_regobject_name_with_control(&input, &control)
            .unwrap()
            .unwrap();
        assert_eq!(*names, [expected]);
        assert_eq!(budget.used(), names.reserved_bytes());
        assert!(budget.peak() <= 256);
        drop(names);
        assert_eq!(budget.used(), 0);
    }
    assert!(parse_regobject_name(&format!("\"{}", "A".repeat(32768))).is_none());
}

#[test]
fn regobject_name_admission_releases_partial_components_on_failure() {
    let input = "pg_catalog.\"a\"\"b\".name";
    let mut rejected = false;
    let mut admitted = false;
    for allowance in [0, 8, 32, 64, 128, 256, 512] {
        let budget = MemoryBudget::new(allowance);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        match parse_regobject_name_with_control(input, &control) {
            Ok(Some(names)) => {
                assert_eq!(*names, ["pg_catalog", "a\"b", "name"]);
                admitted = true;
            }
            Err(error) => {
                assert_eq!(crate::SQLError::from(error).sqlstate(), Some("53200"));
                rejected = true;
            }
            Ok(None) => panic!("valid regobject name"),
        }
        assert!(budget.peak() <= allowance);
        assert_eq!(budget.used(), 0);
    }
    assert!(rejected && admitted);
}

#[test]
fn regobject_name_scanning_checks_both_cancellation_owners_before_allocation() {
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(0);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = parse_regobject_name_with_control(
            "  \"C\"",
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(crate::SQLError::from(error).sqlstate(), Some("57014"));
        assert_eq!(budget.peak(), 0);
    }
}
