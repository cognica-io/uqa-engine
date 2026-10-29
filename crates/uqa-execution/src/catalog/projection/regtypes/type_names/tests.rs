//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    cache::RegtypeOutputCache,
    security::BoundSchemaSecurity,
    test_support::{empty_catalog, CatalogServices},
    CatalogReadView,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, RelationIdentity,
};
use uqa_sql::{
    ast::{CreateDomain, UserTypeIdentity, UserTypeKind},
    catalog::{
        domain::StoredDomain,
        roles::{RoleDefinition, RoleIdentity},
    },
};

pub(super) fn fixture() -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas = Arc::new(BoundSchemaSecurity::initial_catalog());
    snapshot.definitions.roles = Arc::new(BTreeMap::from([(
        "uqa".into(),
        RoleDefinition::bootstrap(),
    )]));
    let domain = StoredDomain {
        object_id: [1; 16],
        oid: 20001,
        array_oid: Some(20002),
        identity: RelationIdentity::new("public", "budget_domain"),
        owner: RoleIdentity::BOOTSTRAP,
        definition: CreateDomain {
            name: "public.budget_domain".into(),
            base: ColumnType::Array(Box::new(ColumnType::Varchar(Some(12)))),
            collation: None,
            collation_oid: Some(100),
            default: None,
            not_null: None,
            checks: Vec::new(),
        },
        array_name: Some("_budget_domain".into()),
        usage_acl: None,
    };
    snapshot.definitions.domains =
        Arc::new(BTreeMap::from([("public.budget_domain".into(), domain)]));
    CatalogReadView::new(snapshot)
}

#[test]
fn catalog_type_projection_matches_builtin_domain_array_and_missing_name_diagnostics() {
    let catalog = fixture();
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    for name in [
        "integer",
        "pg_catalog.int4",
        "varchar(7)[]",
        "budget_domain",
        "public.budget_domain",
        "public._budget_domain",
        "budget_domain[][]",
        "budget_domain(3)",
        "missing_schema.missing",
        "missing_type",
        "database.public.item",
    ] {
        let expected = crate::catalog::projection::resolve_catalog_column_type_name(&context, name);
        let budget = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let actual = resolve_catalog_column_type_name_with_control(&context, name, &control);
        match (&expected, &actual) {
            (Ok(expected), Ok(actual)) => assert_eq!(expected, &**actual, "{name}"),
            (Err(expected), Err(actual)) => {
                assert_eq!(expected.sqlstate(), actual.sqlstate(), "{name}");
                assert_eq!(expected.to_string(), actual.to_string(), "{name}");
            }
            _ => panic!("{name}: {expected:?} != {actual:?}"),
        }
        drop(actual);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn catalog_type_identity_admits_only_selected_type_and_retains_exact_live_storage() {
    let catalog = fixture();
    let identity = UserTypeIdentity {
        kind: UserTypeKind::Domain,
        oid: 20001,
        dimensions: 2,
    };
    let budget = MemoryBudget::new(8192);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = catalog_user_type_identity_with_control(&catalog, identity, &control).unwrap();
    });
    assert_eq!(measured.bytes_current, budget.used() as i64);
    assert_eq!(budget.used(), output.as_ref().unwrap().reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
    for available in [0, 5, 24, 50] {
        let budget = MemoryBudget::new(available);
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        assert_eq!(
            catalog_user_type_identity_with_control(&catalog, identity, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn catalog_type_display_preserves_format_type_names_and_releases_temporary_metadata() {
    let catalog = fixture();
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    for name in [
        "integer",
        "char(3)",
        "varchar(7)",
        "bit(4)",
        "numeric(8,2)",
        "timestamp(3)",
        "pg_catalog.\"char\"",
        "budget_domain",
        "budget_domain[]",
        "int2vector",
        "int4range",
    ] {
        let ty =
            crate::catalog::projection::resolve_catalog_column_type_name(&context, name).unwrap();
        let expected = crate::catalog::projection::format_type_name(&context, &ty).unwrap();
        let budget = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let mut result = None;
        let measured = allocation_counter::measure(|| {
            result = Some(
                crate::catalog::projection::format_type_name_with_control(&context, &ty, &control)
                    .unwrap(),
            );
        });
        assert_eq!(result.as_ref().unwrap().as_str(), expected, "{name}");
        assert_eq!(measured.bytes_current, budget.used() as i64);
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}
