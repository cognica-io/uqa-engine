//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn coercion_preserves_domain_identity_or_selects_its_base() {
    let control = ProductionControl::uncontrolled();
    let base = ColumnType::Array(Box::new(ColumnType::Integer));
    let domain = ColumnType::Domain {
        collation_oid: None,
        schema: "public".into(),
        name: "int_list".into(),
        oid: 90001,
        array_oid: None,
        base: Box::new(base.clone()),
    };
    for target in [P::Any, P::AnyElement, P::AnyCompatible] {
        assert_eq!(
            select(&ColumnType::Pseudo(target), Some(&domain), false, &control).unwrap(),
            Some(Coercion::Preserve)
        );
    }
    assert_eq!(
        select(&ColumnType::AnyArray, Some(&domain), false, &control).unwrap(),
        Some(Coercion::Relabel(&base))
    );
    let error = select(
        &ColumnType::Pseudo(P::AnyNonArray),
        Some(&domain),
        false,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42846"));
}

#[test]
fn unknown_constants_and_computed_values_use_distinct_coercions() {
    let control = ProductionControl::uncontrolled();
    let unknown = ColumnType::Pseudo(P::Unknown);
    assert_eq!(
        select(&unknown, None, true, &control).unwrap(),
        Some(Coercion::Preserve)
    );
    assert_eq!(
        select(&unknown, Some(&ColumnType::Text), false, &control).unwrap(),
        Some(Coercion::Unknown)
    );
    let target = ColumnType::Pseudo(P::AnyRange);
    assert_eq!(
        select(&target, None, false, &control).unwrap(),
        Some(Coercion::Relabel(&target))
    );
    let error = select(&target, None, true, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(error.to_string(), "cannot accept a value of type anyrange");
}

#[test]
fn cancellation_precedes_polymorphic_selection() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    token.cancel();
    let error = select(&ColumnType::Pseudo(P::AnyElement), None, false, &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
}

#[test]
fn expression_type_names_do_not_select_physical_scalar_operators() {
    let control = ProductionControl::uncontrolled();
    for pseudo in [P::Unknown, P::Any, P::AnyEnum, P::AnyRange] {
        let name = pseudo.name();
        let catalog = format!("pg_catalog.\"{name}\"");
        assert_eq!(
            *ColumnType::from_expression_type_name_with_control(&catalog, &control).unwrap(),
            ColumnType::Pseudo(pseudo)
        );
        assert!(ColumnType::from_sql_name(&catalog).is_err());
        assert!(crate::catalog::type_metadata::builtin_scalar_type(pseudo as u32).is_none());
        assert!(crate::catalog::type_metadata::builtin_scalar_type_by_name(name).is_none());
    }
}
