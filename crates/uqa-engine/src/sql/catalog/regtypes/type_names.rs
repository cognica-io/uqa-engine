//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-backed SQL type-name resolution.

use std::sync::LazyLock;

use uqa_sql::ast::ColumnType;

use crate::Engine;

use super::super::schema;

static CATALOG_DOMAIN_TYPES: LazyLock<Vec<ColumnType>> = LazyLock::new(|| {
    let mut domains = schema::information_schema_domains();
    domains.extend(schema::ag_catalog_domains());
    domains
});

pub(crate) fn resolve_catalog_column_type(engine: &Engine, type_name: &str) -> Option<ColumnType> {
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
    let mut resolved = CATALOG_DOMAIN_TYPES
        .iter()
        .find(|domain| match domain {
            ColumnType::Domain {
                schema: domain_schema,
                name: domain_name,
                ..
            } => {
                domain_name == local_name
                    && schema.map_or_else(
                        || engine.search_path_contains(domain_schema),
                        |schema| domain_schema == schema,
                    )
            }
            _ => false,
        })
        .cloned();
    if let Some(ty) = resolved.as_mut() {
        for _ in 0..array_dimensions {
            *ty = ColumnType::Array(Box::new(ty.clone()));
        }
    }
    resolved
}
