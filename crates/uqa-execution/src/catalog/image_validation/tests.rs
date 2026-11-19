//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod constraints;
mod indexes;
mod partitions;
mod routine_types;
use crate::catalog::test_support::{empty_catalog, table_snapshot};
use uqa_core::{memory::RetainedValue, RelationIdentity};
use uqa_sql::{
    ast::{ColumnDef, TableConstraintSet},
    catalog::{
        roles::{RoleDefinition, RoleDefinitions},
        security::{BoundSchemaSecurity, BoundTableSecurity},
    },
};

pub(super) fn snapshot() -> CatalogReadSnapshot {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.roles = RetainedValue::new(RoleDefinitions::initial_catalog());
    snapshot.definitions.schemas = RetainedValue::new(BoundSchemaSecurity::initial_catalog());
    snapshot
}

pub(super) fn windows_until_rejected(
    catalog: &crate::catalog::CatalogReadView,
    steps: usize,
    diagnostic: &str,
) -> usize {
    use crate::catalog::test_support::NoRoutines;
    let memory = MemoryBudget::new(16 << 20);
    let cancel = CancellationToken::new();
    let mut validation = RelationshipValidation::new(catalog.clone(), &memory, &cancel);
    for window in 1..=2_000_000 {
        match validation.advance_with_steps(&NoRoutines, &NoRoutines, steps) {
            Ok(Some(next)) => validation = next,
            Ok(None) => panic!("accepted a missing relationship"),
            Err(error) => {
                assert!(error.to_string().contains(diagnostic), "{error}");
                assert_eq!(memory.used(), 0);
                return window;
            }
        }
    }
    panic!("relationship validation did not complete")
}

/// Drive the same retained lookup-service entry used by public memory and durable restore.
fn retained_binding_windows(
    catalog: &crate::catalog::CatalogReadView,
    memory: &MemoryBudget,
    token: &CancellationToken,
    steps: usize,
    stop: Option<usize>,
) -> (usize, Result<(), SQLError>) {
    use super::routine_declarations::BorrowedTypes;
    use crate::catalog::test_support::NoRoutines;
    let owner = std::sync::Arc::new(NoRoutines);
    let mut validation = RelationshipValidation::new(catalog.clone(), memory, token);
    for window in 1..=2_000_000 {
        if stop == Some(window) {
            token.cancel();
        }
        let next = if window.is_multiple_of(2) && std::sync::Arc::strong_count(&owner) > 1 {
            validation.advance_with_steps(owner.as_ref(), owner.as_ref(), steps)
        } else {
            validation.advance_with_retained_services(
                &owner,
                std::sync::Arc::as_ref,
                &BorrowedTypes(owner.as_ref()),
                steps,
            )
        };
        let result = match next {
            Ok(Some(next)) => {
                validation = next;
                continue;
            }
            Ok(None) => Ok(()),
            Err(error) => Err(error),
        };
        assert_eq!(std::sync::Arc::strong_count(&owner), 1);
        return (window, result);
    }
    panic!("catalog validation did not complete");
}

#[test]
fn retained_default_binding_reaches_late_scalar_nodes_and_releases_the_session() {
    use crate::catalog::CatalogReadView;
    use uqa_sql::ast::Expr;
    const WIDTH: usize = 512;
    let memory = MemoryBudget::new(2 << 20);
    let input = |bad: Option<(usize, Expr)>| {
        let mut snapshot = snapshot();
        let mut column = ColumnDef::nullable(
            "values",
            uqa_sql::ColumnType::Array(Box::new(uqa_sql::ColumnType::BigInteger)),
        );
        let mut nodes = vec![Expr::Literal(uqa_core::Value::Int(1)); WIDTH];
        if let Some((position, expression)) = bad {
            nodes[position] = expression;
        }
        column.default = Some(Expr::Array(nodes));
        snapshot.tables.insert(
            RelationIdentity::new("public", "saved"),
            table_snapshot([1; 16], vec![column], TableConstraintSet::default()),
        );
        CatalogReadView::new(snapshot)
    };
    for steps in [1, 32] {
        for (invalid, diagnostic) in [
            (
                Expr::Default,
                "invalid native default expression binding: non-scalar expression",
            ),
            (
                Expr::Column("unknown_field".into()),
                "column \"unknown_field\" does not exist",
            ),
        ] {
            let reject = |position| {
                let (windows, result) = retained_binding_windows(
                    &input(Some((position, invalid.clone()))),
                    &memory,
                    &CancellationToken::new(),
                    steps,
                    None,
                );
                let error = result.unwrap_err();
                assert!(error.to_string().contains(diagnostic), "{error}");
                assert_eq!(memory.used(), 0);
                windows
            };
            let first = reject(0);
            let last = reject(WIDTH - 1);
            eprintln!(
                "stored default {diagnostic}, {steps} steps: first {first}, last {last}, peak {}",
                memory.peak()
            );
            assert!(
                last >= first + (WIDTH - 1) / steps,
                "{steps} steps: first {first}, last {last}"
            );
        }
    }
    let (_, result) =
        retained_binding_windows(&input(None), &memory, &CancellationToken::new(), 64, None);
    result.unwrap();
    assert_eq!(memory.used(), 0);
    let denied = MemoryBudget::new(0);
    let (_, result) =
        retained_binding_windows(&input(None), &denied, &CancellationToken::new(), 64, None);
    assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(denied.used(), 0);
}

#[test]
fn retained_table_checks_coerce_each_boolean_operand_in_its_original_position() {
    use crate::catalog::CatalogReadView;
    use uqa_sql::ast::Expr;
    const WIDTH: usize = 256;
    let memory = MemoryBudget::new(2 << 20);
    for (name, chain) in [
        ("AND", Expr::And as fn(Vec<Expr>) -> Expr),
        ("OR", Expr::Or),
    ] {
        let input = |bad: Option<usize>| {
            let mut source = snapshot();
            let mut nodes = vec![Expr::Literal(uqa_core::Value::Bool(true)); WIDTH];
            if let Some(position) = bad {
                nodes[position] = Expr::Literal(uqa_core::Value::Int(1));
            }
            let mut column = ColumnDef::nullable("v", uqa_sql::ColumnType::BigInteger);
            column.check = Some(chain(nodes));
            source.tables.insert(
                RelationIdentity::new("public", "saved"),
                table_snapshot([1; 16], vec![column], TableConstraintSet::default()),
            );
            CatalogReadView::new(source)
        };
        for steps in [1, 32] {
            let reject = |position| {
                let (windows, result) = retained_binding_windows(
                    &input(Some(position)),
                    &memory,
                    &CancellationToken::new(),
                    steps,
                    None,
                );
                let error = result.unwrap_err();
                assert_eq!(error.sqlstate(), Some("42804"), "{error}");
                assert!(error.to_string().contains(name), "{error}");
                drop(error);
                assert_eq!(memory.used(), 0);
                windows
            };
            let first = reject(0);
            let last = reject(WIDTH - 1);
            assert!(
                last >= first + (WIDTH - 1) / steps,
                "{name} width {steps}: {first} vs {last}"
            );
            let (_, cancelled) = retained_binding_windows(
                &input(Some(WIDTH - 1)),
                &memory,
                &CancellationToken::new(),
                steps,
                Some(first + 2),
            );
            assert_eq!(cancelled.unwrap_err().sqlstate(), Some("57014"));
            assert_eq!(memory.used(), 0);
        }
        retained_binding_windows(&input(None), &memory, &CancellationToken::new(), 32, None)
            .1
            .unwrap();
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn a_small_stored_default_fits_the_catalog_validation_allowance() {
    use crate::catalog::{test_support::NoRoutines, CatalogReadView};
    let mut source = snapshot();
    let mut column = ColumnDef::nullable("value", uqa_sql::ColumnType::BigInteger);
    column.default = Some(uqa_sql::ast::Expr::Literal(uqa_core::Value::Int(1)));
    source.tables.insert(
        RelationIdentity::new("public", "saved"),
        table_snapshot([1; 16], vec![column], TableConstraintSet::default()),
    );
    let catalog = CatalogReadView::new(source);
    let token = CancellationToken::new();
    for allowance in [20 << 10, 24 << 10] {
        let ordinary = MemoryBudget::new(allowance);
        let mut cursor = RelationshipValidation::new(catalog.clone(), &ordinary, &token);
        while let Some(next) = cursor
            .advance_with_steps(&NoRoutines, &NoRoutines, 1)
            .unwrap()
        {
            cursor = next;
        }
        let retained = MemoryBudget::new(allowance);
        retained_binding_windows(&catalog, &retained, &token, 1, None)
            .1
            .unwrap();
        assert_eq!((ordinary.used(), retained.used()), (0, 0));
    }
}

#[test]
fn native_catalog_authority_rejects_missing_and_recreated_table_owners() {
    let mut snapshot = snapshot();
    let memory = MemoryBudget::new(64 << 10);
    let cancellation = CancellationToken::new();
    let mut role = RoleDefinition::bootstrap();
    role.oid = 20000;
    role.object_id = [20; 16];
    role.name = "owner".into();
    RetainedValue::make_mut(&mut snapshot.definitions.roles)
        .insert(role.name.clone(), role.clone())
        .unwrap();
    let mut table = table_snapshot(
        [1; 16],
        vec![ColumnDef::nullable("value", uqa_sql::ColumnType::Integer)],
        TableConstraintSet::default(),
    );
    table.security = std::sync::Arc::new(BoundTableSecurity::owner(role.identity())).into();
    snapshot
        .tables
        .insert(RelationIdentity::new("public", "items"), table);
    validate_authority(&snapshot, &memory, &cancellation).unwrap();
    RetainedValue::make_mut(&mut snapshot.definitions.roles)
        .remove("owner")
        .unwrap();
    assert!(validate_authority(&snapshot, &memory, &cancellation).is_err());
    role.object_id = [21; 16];
    RetainedValue::make_mut(&mut snapshot.definitions.roles)
        .insert(role.name.clone(), role)
        .unwrap();
    assert!(validate_authority(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_catalog_authority_validation_is_read_only_and_cancellation_releases_workspace() {
    let snapshot = snapshot();
    let memory = MemoryBudget::new(64 << 10);
    let cancellation = CancellationToken::new();
    validate_authority(&snapshot, &memory, &cancellation).unwrap();
    assert_eq!(memory.used(), 0);
    assert_eq!(snapshot.definitions.roles.len(), 1);
    cancellation.cancel();
    assert!(validate_authority(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
}

fn typed_snapshot() -> CatalogReadSnapshot {
    let mut snapshot = snapshot();
    let definition = uqa_sql::catalog::enum_type::StoredEnum {
        object_id: [7; 16],
        oid: 25_000,
        array_oid: 25_001,
        array_name: "_mood".into(),
        identity: RelationIdentity::new("public", "mood"),
        owner: uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
        labels: Vec::new(),
        usage_acl: None,
    };
    let table = table_snapshot(
        [8; 16],
        vec![ColumnDef::nullable("value", definition.column_type())],
        TableConstraintSet::default(),
    );
    RetainedValue::make_mut(&mut snapshot.definitions.enums)
        .insert("public.mood".into(), definition)
        .unwrap();
    snapshot
        .tables
        .insert(RelationIdentity::new("public", "items"), table);
    snapshot
}

#[test]
fn native_catalog_types_reject_missing_and_replaced_column_type_identities() {
    let mut snapshot = typed_snapshot();
    let memory = MemoryBudget::new(64 << 10);
    let cancellation = CancellationToken::new();
    validate_types(&snapshot, &memory, &cancellation).unwrap();
    let mut definition = snapshot
        .definitions
        .enums
        .get("public.mood")
        .unwrap()
        .clone();
    RetainedValue::make_mut(&mut snapshot.definitions.enums)
        .remove("public.mood")
        .unwrap();
    assert!(validate_types(&snapshot, &memory, &cancellation).is_err());
    definition.oid += 2;
    definition.array_oid += 2;
    RetainedValue::make_mut(&mut snapshot.definitions.enums)
        .insert("public.mood".into(), definition)
        .unwrap();
    assert!(validate_types(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_catalog_types_reject_cross_owner_row_type_collisions() {
    let mut snapshot = typed_snapshot();
    let memory = MemoryBudget::new(64 << 10);
    let cancellation = CancellationToken::new();
    let table = snapshot
        .tables
        .get_mut(&RelationIdentity::new("public", "items"))
        .unwrap();
    table.catalog_oids.row_type = Some(25_001);
    assert!(validate_types(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_catalog_types_retire_workspace_on_quota_denial_and_cancellation() {
    let snapshot = typed_snapshot();
    let cancellation = CancellationToken::new();
    let empty = MemoryBudget::new(0);
    assert!(validate_types(&snapshot, &empty, &cancellation).is_err());
    let memory = MemoryBudget::new(64 << 10);
    cancellation.cancel();
    assert!(validate_types(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn a_small_subquery_rule_fits_the_ordinary_catalog_validation_allowance() {
    use crate::catalog::{test_support::NoRoutines, CatalogReadView};
    use uqa_sql::{
        ast::{EventEnableMode, Statement},
        catalog::events::{RuleConditionBinding, RuleDependencies, StoredRule},
        plan::ExpressionPlan,
    };
    let mut source = snapshot();
    let owner = RelationIdentity::new("public", "saved");
    source.tables.insert(
        owner.clone(),
        table_snapshot(
            [1; 16],
            vec![ColumnDef::nullable(
                "value",
                uqa_sql::ColumnType::BigInteger,
            )],
            TableConstraintSet::default(),
        ),
    );
    let Statement::CreateRule(definition) = uqa_sql::compile(
        "CREATE RULE saved_check AS ON UPDATE TO public.saved WHERE EXISTS (VALUES (1)) DO NOTHING",
    )
    .unwrap()
    .remove(0) else {
        panic!("rule")
    };
    let plan = ExpressionPlan::lower(definition.condition.clone().unwrap());
    let binding = RuleConditionBinding::for_event(&["value".into()], definition.event);
    RetainedValue::make_mut(&mut source.definitions.rules)
        .insert_event(
            owner,
            "saved_check".into(),
            StoredRule {
                definition,
                condition_plan: Some(plan),
                condition_binding: Some(binding),
                enabled: EventEnableMode::default(),
                dependencies: Some(RuleDependencies::default()),
                catalog_oid: Some(31_000),
            },
        )
        .unwrap();
    let catalog = CatalogReadView::new(source);
    let token = CancellationToken::new();
    for kib in [8, 12, 16, 20, 24, 32, 64] {
        let ordinary = MemoryBudget::new(kib << 10);
        let mut cursor = RelationshipValidation::new(catalog.clone(), &ordinary, &token);
        let expected = loop {
            match cursor.advance_with_steps(&NoRoutines, &NoRoutines, 1) {
                Ok(Some(next)) => cursor = next,
                finished => {
                    break finished
                        .map(|_| ())
                        .map_err(|error| error.sqlstate().unwrap().to_owned())
                }
            }
        };
        assert_eq!(expected.is_ok(), kib >= 20, "{kib} KiB");
        let retained = MemoryBudget::new(kib << 10);
        let actual = retained_binding_windows(&catalog, &retained, &token, 1, None)
            .1
            .map_err(|error| error.sqlstate().unwrap().to_owned());
        assert_eq!(expected, actual, "{kib} KiB");
        assert_eq!((ordinary.used(), retained.used()), (0, 0));
    }
}
