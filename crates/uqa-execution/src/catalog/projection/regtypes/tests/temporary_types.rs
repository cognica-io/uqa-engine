//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A temporary type reference follows the invoking session, including array syntax and quoted names.

use crate::catalog::{cache::RegtypeOutputCache, test_support, CatalogReadView};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::RelationIdentity;
use uqa_sql::{catalog::domain::StoredDomain, ColumnType};

#[test]
fn qualified_temporary_types_follow_the_session_namespace() {
    let domains: BTreeMap<_, _> = [1_u32, 2]
        .into_iter()
        .map(|id| {
            let schema = format!("pg_temp_{id}");
            let name = format!("{schema}.object");
            let uqa_sql::Statement::CreateDomain(definition) =
                uqa_sql::compile(&format!("CREATE DOMAIN {name} AS integer"))
                    .unwrap()
                    .remove(0)
            else {
                unreachable!()
            };
            (
                name,
                StoredDomain {
                    object_id: [u8::try_from(id).unwrap(); 16],
                    oid: 20_000 + id,
                    array_oid: Some(30_000 + id),
                    identity: RelationIdentity::new(schema, "object"),
                    owner: uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
                    definition,
                    array_name: None,
                    usage_acl: None,
                },
            )
        })
        .collect();
    let mut snapshot = test_support::empty_catalog().snapshot().clone();
    snapshot.definitions.domains = Arc::new(domains.clone());
    let catalog = CatalogReadView::new(snapshot);
    let cache = RegtypeOutputCache::default();
    for id in [1, 2] {
        let mut services = test_support::CatalogServices::default();
        services.resolution.temporary_schema = format!("pg_temp_{id}");
        services.resolution.temporary_namespace_allocated = true;
        let context = services.context(&catalog, &cache);
        let expected = domains[&format!("pg_temp_{id}.object")].column_type();
        for name in ["pg_temp.object", "\"pg_temp\".\"object\""] {
            assert_eq!(
                super::super::resolve_catalog_column_type(&context, name),
                Some(expected.clone())
            );
            assert_eq!(
                super::super::resolve_catalog_column_type(&context, &format!("{name}[]")),
                Some(ColumnType::Array(Box::new(expected.clone())))
            );
        }
        assert_eq!(
            super::super::resolve_catalog_column_type(&context, "pg_temp_1.object"),
            Some(domains["pg_temp_1.object"].column_type())
        );
    }
}
