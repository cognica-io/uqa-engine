//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::catalog::{roles::RoleDefinition, security::BoundSchemaSecurity};

fn resolution() -> RelationNameResolution {
    RelationNameResolution {
        search_path: vec!["private".into(), "public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "reader".into(),
        lookup_mode: RelationLookupMode::Dynamic,
    }
}

#[test]
fn admitted_relation_resolution_preserves_namespace_usage_and_bound_identity_order() {
    let mut snapshot = crate::catalog::test_support::empty_catalog()
        .snapshot()
        .clone();
    let mut reader = RoleDefinition::bootstrap();
    reader.name = "reader".into();
    reader.oid = 42;
    reader.object_id = [42; 16];
    reader.attributes.clear();
    snapshot.definitions.roles = Arc::new(
        [
            ("uqa".into(), RoleDefinition::bootstrap()),
            ("reader".into(), reader),
        ]
        .into(),
    );
    snapshot.definitions.schemas = Arc::new(
        [
            (
                "public".into(),
                BoundSchemaSecurity::with_public_privileges(true),
            ),
            ("private".into(), BoundSchemaSecurity::bootstrap("private")),
            (
                "odd.schema".into(),
                BoundSchemaSecurity::with_public_privileges(true),
            ),
        ]
        .into(),
    );
    let tables = [
        RelationIdentity::new("private", "items"),
        RelationIdentity::new("public", "items"),
        RelationIdentity::new("odd.schema", "odd\"table"),
        RelationIdentity::new("pg_temp_1", "temporary"),
    ]
    .into();
    let catalog = RelationNameCatalog::new(&tables as &BTreeSet<_>, &snapshot.definitions);
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut resolution = resolution();
    for (name, expected) in [
        (
            "items",
            RelationResolution::Found("public.items".into(), "table"),
        ),
        (
            "missing.items",
            RelationResolution::MissingSchema("missing".into()),
        ),
        (
            "pg_temp.temporary",
            RelationResolution::MissingSchema("pg_temp".into()),
        ),
        ("public.absent", RelationResolution::MissingRelation),
        (
            "\"odd.schema\".\"odd\"\"table\"",
            RelationResolution::Found("\"odd.schema\".\"odd\"\"table\"".into(), "table"),
        ),
    ] {
        let result = catalog
            .relation_kind_resolution_with_control(&resolution, name, &control)
            .unwrap();
        assert_eq!(*result, expected);
        assert_eq!(budget.used(), result.reserved_bytes());
        drop(result);
        assert_eq!(budget.used(), 0);
    }
    let error = catalog
        .relation_kind_resolution_with_control(&resolution, "private.items", &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    assert_eq!(error.to_string(), "permission denied for schema private");
    resolution.lookup_mode = RelationLookupMode::Bound;
    let result = catalog
        .relation_kind_resolution_with_control(&resolution, "private.items", &control)
        .unwrap();
    assert_eq!(
        *result,
        RelationResolution::Found("private.items".into(), "table")
    );
    drop(result);
    assert_eq!(
        catalog
            .relation_kind_resolution_with_control(&resolution, "items", &control)
            .unwrap_err()
            .to_string(),
        "internal error: bound query contains non-canonical relation reference `items`"
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn relation_output_retains_only_its_exact_selected_name_buffer() {
    let snapshot = crate::catalog::test_support::empty_catalog();
    let tables = [RelationIdentity::new("public", "items")].into();
    let catalog =
        RelationNameCatalog::new(&tables as &BTreeSet<_>, &snapshot.snapshot().definitions);
    let mut resolution = resolution();
    resolution.lookup_mode = RelationLookupMode::Bound;
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    control.check().unwrap();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(
            catalog
                .relation_kind_resolution_with_control(&resolution, "public.items", &control)
                .unwrap(),
        );
    });
    let output = output.unwrap();
    let RelationResolution::Found(name, "table") = &*output else {
        panic!("selected table");
    };
    assert_eq!(output.reserved_bytes(), name.capacity());
    assert_eq!(allocations.bytes_current, budget.used() as i64);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn relation_candidate_rejection_and_both_cancellation_handles_release_scratch() {
    let snapshot = crate::catalog::test_support::empty_catalog();
    let tables = BTreeSet::<RelationIdentity>::new();
    let catalog = RelationNameCatalog::new(&tables, &snapshot.snapshot().definitions);
    let mut resolution = resolution();
    resolution.lookup_mode = RelationLookupMode::Bound;
    let name = format!("public.{}", "long_relation".repeat(64 * 1024));
    let budget = MemoryBudget::new(512);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    control.check().unwrap();
    let sibling = budget.reserve(7).unwrap();
    let allocations = allocation_counter::measure(|| {
        assert_eq!(
            catalog
                .relation_kind_resolution_with_control(&resolution, &name, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocations.bytes_total < 16 * 1024, "{allocations:?}");
    assert_eq!(budget.used(), 7);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            catalog
                .relation_kind_resolution_with_control(&resolution, &name, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 7);
        token.reset();
    }
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
