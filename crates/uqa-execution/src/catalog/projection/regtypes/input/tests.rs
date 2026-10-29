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
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::catalog::roles::RoleDefinition;

fn fixture() -> CatalogReadView {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas = Arc::new(BoundSchemaSecurity::initial_catalog());
    snapshot.definitions.roles = Arc::new(BTreeMap::from([(
        "uqa".into(),
        RoleDefinition::bootstrap(),
    )]));
    let uqa_sql::Statement::CreateFunction(mut definition) = uqa_sql::compile(
        "CREATE FUNCTION unrelated(text) RETURNS integer LANGUAGE SQL AS 'SELECT 1'",
    )
    .unwrap()
    .remove(0) else {
        panic!("function");
    };
    definition.name = "public.unrelated".into();
    definition.catalog_oid = Some(20001);
    definition.object_id = Some([1; 16]);
    definition.owner = Some(uqa_core::catalog_role::RoleIdentity::BOOTSTRAP);
    definition.body = uqa_sql::ast::FunctionBody::Source("x".repeat(4 << 20));
    let function = uqa_sql::routines::SQLUserFunction::new(
        *definition,
        uqa_sql::routines::RoutineBody::Source,
    );
    snapshot.definitions.sql_user_functions = Arc::new(BTreeMap::from([(
        "public.unrelated".into(),
        vec![Arc::new(function)],
    )]));
    CatalogReadView::new(snapshot)
}

fn ordinary(
    context: &CatalogContext<'_>,
    ty: &ColumnType,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    use super::super as regtypes;
    match ty {
        ColumnType::Regclass => regtypes::resolve_regclass_oid(context, name),
        ColumnType::Regtype => regtypes::resolve_regtype_oid(context, name),
        ColumnType::Regproc => regtypes::resolve_regproc_input_oid(context, name).map(Some),
        ColumnType::Regprocedure => {
            regtypes::procedures::resolve_regprocedure_input_oid(context, name).map(Some)
        }
        ColumnType::Regnamespace => regtypes::resolve_regnamespace_oid(context, name),
        ColumnType::Regcollation => regtypes::resolve_regcollation_oid(context, name),
        ColumnType::Regrole => regtypes::resolve_regrole_oid(context, name),
        _ => unreachable!(),
    }
}

#[test]
fn selected_alias_input_preserves_existing_names_signatures_and_diagnostics() {
    let catalog = fixture();
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    let token = CancellationToken::new();
    for (ty, name) in [
        (ColumnType::Regclass, "pg_catalog.pg_class"),
        (ColumnType::Regclass, "missing_name"),
        (ColumnType::Regclass, "other.public.table"),
        (ColumnType::Regtype, "integer"),
        (ColumnType::Regtype, "integer[][]"),
        (ColumnType::Regtype, "pg_catalog._int4[]"),
        (ColumnType::Regtype, "cstring"),
        (ColumnType::Regtype, "trigger"),
        (ColumnType::Regtype, "pg_catalog.pg_language"),
        (ColumnType::Regtype, "missing_type"),
        (ColumnType::Regtype, "missing_schema.integer"),
        (ColumnType::Regproc, "octet_length"),
        (ColumnType::Regproc, "unrelated"),
        (ColumnType::Regproc, "missing_schema.missing_function"),
        (ColumnType::Regprocedure, "octet_length(bytea)"),
        (ColumnType::Regprocedure, "unrelated(text)"),
        (ColumnType::Regprocedure, "uqa.public.unrelated(text)"),
        (ColumnType::Regprocedure, "missing_function(integer)"),
        (ColumnType::Regnamespace, "pg_catalog"),
        (ColumnType::Regnamespace, "missing_schema"),
        (ColumnType::Regcollation, "\"C\""),
        (ColumnType::Regcollation, "missing_schema.missing_collation"),
        (ColumnType::Regrole, "uqa"),
        (ColumnType::Regrole, "missing_role"),
        (ColumnType::Regclass, "08"),
        (ColumnType::Regtype, "4294967296"),
        (ColumnType::Regprocedure, "-"),
    ] {
        let expected = ordinary(&context, &ty, name);
        let memory = MemoryBudget::new(1 << 20);
        let actual = resolve_oid_alias_input_with_control(
            &context,
            &ty,
            name,
            &ProductionControl::new(&memory, &token, &token),
        );
        match (expected, actual) {
            (Ok(left), Ok(right)) => assert_eq!(left, right, "{ty:?} {name}"),
            (Err(left), Err(right)) => {
                assert_eq!(left.sqlstate(), right.sqlstate(), "{ty:?} {name}");
                assert_eq!(left.to_string(), right.to_string(), "{ty:?} {name}");
            }
            (left, right) => panic!("{ty:?} {name}: {left:?} != {right:?}"),
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn selected_alias_input_avoids_unrelated_catalog_projection_and_releases_lookup_workspace() {
    let catalog = fixture();
    let services = CatalogServices::default();
    let cache = RegtypeOutputCache::default();
    let context = services.context(&catalog, &cache);
    let token = CancellationToken::new();
    for (ty, name) in [
        (ColumnType::Regnamespace, "public"),
        (ColumnType::Regproc, "unrelated"),
        (ColumnType::Regclass, "pg_class"),
    ] {
        let memory = MemoryBudget::new(16384);
        let control = ProductionControl::new(&memory, &token, &token);
        let measured = allocation_counter::measure(|| {
            assert!(
                resolve_oid_alias_input_with_control(&context, &ty, name, &control)
                    .unwrap()
                    .is_some()
            );
        });
        assert!(measured.bytes_total < 16384, "{ty:?}: {measured:?}");
        assert_eq!(measured.bytes_current, 0);
        assert_eq!(memory.used(), 0);
    }
    for original_cancelled in [false, true] {
        let memory = MemoryBudget::new(16384);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        // Establish the thread's cancellation-deferral identity outside the measured producer.
        assert!(ProductionControl::new(&memory, &original, &invoking)
            .check()
            .is_err());
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                resolve_oid_alias_input_with_control(
                    &context,
                    &ColumnType::Regclass,
                    "pg_class",
                    &ProductionControl::new(&memory, &original, &invoking)
                )
                .unwrap_err()
                .sqlstate(),
                Some("57014")
            );
        });
        assert_eq!(measured.count_total, 0, "{measured:?}");
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(0);
    let control = ProductionControl::new(&memory, &token, &token);
    assert_eq!(
        resolve_oid_alias_input_with_control(
            &context,
            &ColumnType::Regnamespace,
            "public",
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    assert_eq!(
        resolve_oid_alias_input_with_control(&context, &ColumnType::Regclass, "42", &control)
            .unwrap(),
        Some(42)
    );
}
