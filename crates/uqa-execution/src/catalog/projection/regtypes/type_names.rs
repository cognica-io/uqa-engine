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

/// Creation reads current type declarations, independently of a query's retained snapshot. Enum array names occupy the type namespace too.
pub fn named_type_exists<'a>(
    mut domains: impl Iterator<Item = &'a uqa_sql::catalog::domain::StoredDomain>,
    mut enums: impl Iterator<Item = &'a uqa_sql::catalog::enum_type::StoredEnum>,
    identity: &uqa_core::RelationIdentity,
) -> bool {
    domains.any(|domain| {
        domain.identity == *identity
            || (domain.identity.schema == identity.schema
                && domain.array_type_name() == identity.name)
    }) || enums.any(|definition| {
        definition.identity.schema == identity.schema
            && (definition.identity.name == identity.name || definition.array_name == identity.name)
    }) || ColumnType::from_sql_name(&identity.qualified_name()).is_ok()
        || CATALOG_NAMED_TYPES.iter().any(|ty| {
            matches!(ty, ColumnType::Domain { schema, name, .. }
                if schema == &identity.schema && name == &identity.name)
        })
}

/// The current definition of the user-defined type an identity names, wrapped in its array dimensions.
pub fn resolve_user_type_identity(
    context: &CatalogContext<'_>,
    identity: uqa_sql::ast::UserTypeIdentity,
) -> Option<ColumnType> {
    catalog_user_type_identity(&context.catalog_read_view(), identity)
}

/// [`resolve_user_type_identity`] over one catalog snapshot.
pub fn catalog_user_type_identity(
    catalog: &crate::catalog::CatalogReadView,
    identity: uqa_sql::ast::UserTypeIdentity,
) -> Option<ColumnType> {
    let mut resolved = match identity.kind {
        uqa_sql::ast::UserTypeKind::Enum => catalog
            .enums()
            .find(|definition| definition.oid == identity.oid)?
            .column_type(),
        uqa_sql::ast::UserTypeKind::Domain => catalog
            .domains()
            .find(|domain| domain.oid == identity.oid)
            .map(uqa_sql::catalog::domain::StoredDomain::column_type)
            .or_else(|| uqa_sql::catalog::system_catalog_domain(identity.oid))?,
    };
    for _ in 0..identity.dimensions {
        resolved = ColumnType::Array(Box::new(resolved));
    }
    Some(resolved)
}

/// The `pg_type` OID of a declared routine type. A user-defined type is named by identity; its array type OID is recorded in the catalog.
pub fn catalog_routine_type_oid(catalog: &crate::catalog::CatalogReadView, type_name: &str) -> i64 {
    let canonical = uqa_sql::type_resolution::canonical_routine_type_name(type_name);
    uqa_sql::ast::UserTypeIdentity::parse(&canonical)
        .and_then(|identity| catalog_user_type_identity(catalog, identity))
        .map_or_else(
            || uqa_sql::catalog::type_metadata::routine_type_oid(type_name),
            |ty| pg_type_oid(&ty),
        )
}

pub fn resolve_catalog_user_type_by_oid(
    context: &CatalogContext<'_>,
    oid: u32,
) -> Option<ColumnType> {
    let catalog = context.catalog_read_view();
    for domain in catalog
        .domains()
        .map(uqa_sql::catalog::domain::StoredDomain::column_type)
        .chain(
            catalog
                .enums()
                .map(uqa_sql::catalog::enum_type::StoredEnum::column_type),
        )
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
    // Stored syntax, routine signatures and bound casts record user-defined types by OID identity rather than by a search-path-dependent name.
    if let Some(identity) = uqa_sql::ast::UserTypeIdentity::parse(type_name) {
        return resolve_user_type_identity(context, identity);
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
    let mut resolved = context.resolve_user_type(base_name).or_else(|| {
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

/// `format_type_be` of a type in one catalog snapshot: a user-defined type is spelled by its current name, qualified when the search path does not include its schema; built-in types use their SQL spelling.
pub fn catalog_type_display_name(
    resolution: &crate::catalog::RelationNameResolution,
    ty: &ColumnType,
) -> String {
    let qualified = |schema: &str, name: &str| {
        let local = uqa_sql::expr::quote_ident(name);
        if schema == "pg_catalog" || resolution.search_path().iter().any(|entry| entry == schema) {
            local
        } else {
            format!("{}.{local}", uqa_sql::expr::quote_ident(schema))
        }
    };
    match ty {
        ColumnType::Array(element) => {
            format!("{}[]", catalog_type_display_name(resolution, element))
        }
        ColumnType::Enum(reference) => qualified(&reference.schema, &reference.name),
        ColumnType::Domain { schema, name, .. } => qualified(schema, name),
        other => other.sql_name(),
    }
}

/// Enum label output over one catalog snapshot, for catalog projections that render stored enum constants.
pub struct CatalogEnumLabels<'a> {
    pub catalog: &'a crate::catalog::CatalogReadView,
    pub resolution: &'a crate::catalog::RelationNameResolution,
}

impl uqa_sql::expr::enums::EnumLabelCatalog for CatalogEnumLabels<'_> {
    fn enum_type_labels(
        &self,
        type_oid: u32,
    ) -> Result<Option<std::sync::Arc<uqa_sql::expr::enums::EnumTypeLabels>>, uqa_sql::SQLError>
    {
        Ok(self
            .catalog
            .enums()
            .find(|definition| definition.oid == type_oid)
            .map(|definition| {
                std::sync::Arc::new(uqa_sql::expr::enums::EnumTypeLabels {
                    type_oid,
                    labels: definition
                        .labels
                        .iter()
                        .map(|label| uqa_sql::expr::enums::EnumTypeLabel {
                            oid: label.oid,
                            key: label.key.clone(),
                            label: label.label.clone(),
                        })
                        .collect(),
                })
            }))
    }

    fn enum_label_uncommitted(&self, _label_oid: u32) -> bool {
        false
    }

    fn enum_type_name(&self, type_oid: u32) -> Result<Option<String>, uqa_sql::SQLError> {
        Ok(self
            .catalog
            .enums()
            .find(|definition| definition.oid == type_oid)
            .map(|definition| {
                catalog_type_display_name(self.resolution, &definition.column_type())
            }))
    }

    fn has_enum_types(&self) -> bool {
        self.catalog.enums().next().is_some()
    }
}
