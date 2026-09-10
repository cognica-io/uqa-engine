//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-backed SQL type-name resolution.

use std::sync::LazyLock;

use uqa_sql::ast::ColumnType;

use crate::catalog::context::CatalogContext;

use super::super::helpers::type_metadata::{pg_type_array_oid, pg_type_oid};
use super::super::schema;

static CATALOG_NAMED_TYPES: LazyLock<Vec<ColumnType>> = LazyLock::new(|| {
    let mut domains = schema::information_schema_domains();
    domains.extend(schema::ag_catalog_domains());
    domains.extend([schema::age_graphid(), schema::age_agtype()]);
    domains
});

pub fn resolve_catalog_domain_type_by_oid(
    context: &CatalogContext<'_>,
    oid: u32,
) -> Option<ColumnType> {
    let catalog = context.catalog_read_view();
    for domain in catalog
        .domains()
        .map(uqa_sql::catalog::domain::StoredDomain::column_type)
        .chain(CATALOG_NAMED_TYPES.iter().cloned())
    {
        if pg_type_oid(&domain) == i64::from(oid) {
            return Some(domain);
        }
        if pg_type_array_oid(&domain) == i64::from(oid) {
            return Some(ColumnType::Array(Box::new(domain)));
        }
    }
    None
}

pub fn resolve_catalog_column_type(
    context: &CatalogContext<'_>,
    type_name: &str,
) -> Option<ColumnType> {
    if let Ok(ty) = ColumnType::from_sql_name(type_name) {
        return Some(ty);
    }
    let mut base_name = type_name.trim();
    let mut array_dimensions = 0usize;
    while let Some(element) = base_name.strip_suffix("[]") {
        base_name = element.trim_end();
        array_dimensions += 1;
    }
    let (schema, local_name) = base_name
        .rsplit_once('.')
        .map_or((None, base_name), |(schema, local_name)| {
            (Some(schema.trim_matches('"')), local_name)
        });
    let local_name = local_name.trim_matches('"');
    let mut resolved = context.resolve_domain_type(base_name).or_else(|| {
        CATALOG_NAMED_TYPES
            .iter()
            .find(|domain| match domain {
                ColumnType::Domain {
                    schema: domain_schema,
                    name: domain_name,
                    ..
                } => {
                    domain_name == local_name
                        && schema.map_or_else(
                            || context.search_path_contains(domain_schema),
                            |schema| domain_schema == schema,
                        )
                }
                _ => false,
            })
            .cloned()
    });
    if let Some(ty) = resolved.as_mut() {
        for _ in 0..array_dimensions {
            *ty = ColumnType::Array(Box::new(ty.clone()));
        }
    }
    resolved
}
