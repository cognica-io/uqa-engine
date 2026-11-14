//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{cache::RegtypeOutputCache, test_support::CatalogServices};
use uqa_sql::catalog::{roles::RoleIdentity, shell_type::StoredShellType};

#[test]
fn retained_routine_type_names_match_original_namespace_shadowing_quoting_and_modifiers() {
    let catalog = fixtures::catalog();
    let mut snapshot = catalog.snapshot().clone();
    for (index, (schema, name)) in [
        ("other", "d"),
        ("private", "d"),
        ("pg_temp_1", "d"),
        ("public", "integer"),
        ("public", "a.b"),
        ("public", "quoted\"name"),
        ("pg_catalog", "int4"),
        ("pg_catalog", "_int4"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut domain = fixtures::domain(schema, name);
        domain.oid += u32::try_from(index + 100).unwrap();
        RetainedValue::make_mut(&mut snapshot.definitions.domains)
            .insert(format!("{schema}.{name}"), domain)
            .unwrap();
    }
    RetainedValue::make_mut(&mut snapshot.definitions.shells)
        .insert(
            "other.e".into(),
            StoredShellType {
                object_id: [9; 16],
                oid: 49000,
                identity: RelationIdentity::new("other", "e"),
                owner: RoleIdentity::BOOTSTRAP,
                usage_acl: None,
            },
        )
        .unwrap();
    let catalog = CatalogReadView::new(snapshot);
    let cache = RegtypeOutputCache::default();
    let scope = Scope::default();
    for role in ["uqa", "reader", "missing"] {
        for allocated in [false, true] {
            let mut services = CatalogServices::default();
            services.resolution.current_user = role.into();
            services.resolution.search_path = ["private", "other", "public", "pg_catalog"]
                .map(str::to_owned)
                .into();
            services.resolution.temporary_namespace_allocated = allocated;
            let context = services.context(&catalog, &cache);
            for name in [
                "d",
                "_d[][]",
                "public.d(7)",
                "private.d",
                "pg_temp.d[]",
                "e",
                "public.e ARRAY",
                "c",
                "r",
                "m",
                "t",
                "view_array[]",
                "foreign_array",
                "integer",
                "\"integer\"",
                "int4",
                "_int4",
                "pg_catalog.int4",
                "\"pg_catalog\".\"_int4\"[]",
                "\"a.b\"",
                "\"quoted\"\"name\"",
                "information_schema.sql_identifier",
                "ag_catalog.graphid",
                "SETOF d",
                "a.b.c",
                "d + 1",
                "\"unterminated",
                "missing",
                "",
            ] {
                let expected = resolve_catalog_column_type(&context, name)
                    .as_ref()
                    .and_then(identity);
                let actual = complete(
                    &mut routine(
                        &scope,
                        catalog.clone(),
                        services.resolution.clone(),
                        name.into(),
                    ),
                    1,
                )
                .unwrap()
                .0;
                assert_eq!(actual, expected, "{role}, {allocated}, {name}");
                scope.assert_released();
            }
        }
    }
}
