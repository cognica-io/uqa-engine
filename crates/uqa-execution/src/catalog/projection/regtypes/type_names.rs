//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-backed SQL type-name resolution.

use std::sync::LazyLock;

use uqa_sql::ast::ColumnType;

use crate::catalog::context::CatalogContext;

use super::super::helpers::type_metadata::pg_type_oid;
use super::super::schema;

mod oid;
pub use oid::resolve_catalog_user_type_by_oid_with_control;
mod input;
mod production;
mod selection;
pub use input::{
    routine_type_name_identity_from_input, routine_variadic_array_from_input, RoutineTypeNamespace,
};
pub use selection::catalog_user_type_name_identity_from_input;
#[cfg(test)]
mod input_tests;
mod resolution;
pub use production::{
    catalog_user_type_identity_with_control, catalog_user_type_name_with_control,
};
pub use resolution::{
    resolve_catalog_column_type_name_with_control, resolve_catalog_column_type_with_control,
};

pub(super) static CATALOG_NAMED_TYPES: LazyLock<Vec<ColumnType>> = LazyLock::new(|| {
    let mut domains = schema::information_schema_domains();
    domains.extend(schema::ag_catalog_domains());
    domains.extend([schema::age_graphid(), schema::age_agtype()]);
    domains
});

/// Creation reads current type declarations, independently of a query's retained snapshot. Enum and composite array names occupy the type namespace too.
pub fn named_type_exists<'a>(
    mut domains: impl Iterator<Item = &'a uqa_sql::catalog::domain::StoredDomain>,
    mut enums: impl Iterator<Item = &'a uqa_sql::catalog::enum_type::StoredEnum>,
    mut composites: impl Iterator<Item = &'a uqa_sql::catalog::composite_type::StoredComposite>,
    mut shells: impl Iterator<Item = &'a uqa_sql::catalog::shell_type::StoredShellType>,
    mut ranges: impl Iterator<Item = &'a uqa_sql::catalog::range_type::StoredRange>,
    identity: &uqa_core::RelationIdentity,
) -> bool {
    shells.any(|definition| definition.identity == *identity)
        || ranges.any(|definition| definition.has_type_name(identity))
        || domains.any(|domain| {
            domain.identity == *identity
                || (domain.identity.schema == identity.schema
                    && domain.array_type_name() == identity.name)
        })
        || enums.any(|definition| {
            definition.identity.schema == identity.schema
                && (definition.identity.name == identity.name
                    || definition.array_name == identity.name)
        })
        || composites.any(|definition| {
            definition.identity.schema == identity.schema
                && (definition.identity.name == identity.name
                    || definition.array_name == identity.name)
        })
        || ColumnType::from_sql_name(&identity.qualified_name()).is_ok()
        || CATALOG_NAMED_TYPES.iter().any(|ty| {
            matches!(ty, ColumnType::Domain { schema, name, .. }
                if schema == &identity.schema && name == &identity.name)
        })
}

/// The current definition of the user-defined type an identity names, wrapped in its array dimensions.
pub fn catalog_user_type_identity(
    catalog: &crate::catalog::CatalogReadView,
    identity: uqa_sql::ast::UserTypeIdentity,
) -> Option<ColumnType> {
    catalog_user_type_identity_with_control(
        catalog,
        identity,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary catalog type projection cannot be cancelled or limited")
    .map(|ty| {
        ty.into_uncontrolled()
            .expect("ordinary catalog type projection")
    })
}

/// The `pg_type` OID of a declared routine type. A user-defined type is named by identity; its array type OID is recorded in the catalog.
pub fn catalog_routine_type_oid(catalog: &crate::catalog::CatalogReadView, type_name: &str) -> i64 {
    catalog_routine_type_oid_with_control(
        catalog,
        type_name,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary catalog routine type identity")
}

pub fn catalog_routine_type_oid_with_control(
    catalog: &crate::catalog::CatalogReadView,
    type_name: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<i64, uqa_sql::SQLError> {
    let canonical =
        uqa_sql::type_resolution::canonical_routine_type_name_with_control(type_name, control)?;
    if let Some(identity) = uqa_sql::ast::UserTypeIdentity::parse(&canonical) {
        if let Some(ty) = catalog_user_type_identity_with_control(catalog, identity, control)? {
            return Ok(pg_type_oid(&ty));
        }
    }
    uqa_sql::catalog::type_metadata::routine_type_oid_with_control(type_name, control)
}

pub fn resolve_catalog_user_type_by_oid(
    context: &CatalogContext<'_>,
    oid: u32,
) -> Option<ColumnType> {
    resolve_catalog_user_type_by_oid_with_control(
        context,
        oid,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary type OID lookup")
    .map(|ty| {
        ty.into_uncontrolled()
            .expect("ordinary type OID descriptor")
    })
}

pub fn resolve_catalog_column_type(
    context: &CatalogContext<'_>,
    type_name: &str,
) -> Option<ColumnType> {
    resolve_catalog_column_type_with_control(
        context,
        type_name,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .ok()
    .flatten()?
    .into_uncontrolled()
    .ok()
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
        ColumnType::Composite(reference) => qualified(&reference.schema, &reference.name),
        ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) => {
            qualified(&reference.schema, &reference.name)
        }
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
    fn value_range_cast_binding(
        &self,
        source: u32,
        target: u32,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<uqa_sql::ast::FunctionBinding>>, uqa_sql::SQLError>
    {
        self.catalog.range_cast_binding(source, target, control)
    }

    fn value_range_type(
        &self,
        oid: u32,
    ) -> Result<Option<std::sync::Arc<uqa_sql::expr::RangeValueDescriptor>>, uqa_sql::SQLError>
    {
        self.catalog.range_value_descriptor(oid)
    }

    fn enum_label_position(
        &self,
        label_oid: u32,
    ) -> Result<Option<(u32, usize)>, uqa_sql::SQLError> {
        Ok(self.catalog.enums().find_map(|definition| {
            definition
                .labels
                .iter()
                .position(|label| label.oid == label_oid)
                .map(|position| (definition.oid, position))
        }))
    }

    fn enum_value_by_oid(
        &self,
        label_oid: u32,
    ) -> Result<Option<uqa_core::EnumValue>, uqa_sql::SQLError> {
        Ok(self.catalog.enums().find_map(|definition| {
            definition
                .labels
                .iter()
                .find(|label| label.oid == label_oid)
                .map(|label| {
                    uqa_core::EnumValue::new(definition.oid, label.key.clone())
                        .with_label_oid(Some(label_oid))
                })
        }))
    }

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

/// Named type input rejects undefined shells; object-lifecycle lookup can retain the same OID without requiring a value layout.
pub(super) fn require_defined(
    catalog: &super::RegtypeOutputCatalog,
    oid: Option<i64>,
    parsed: &uqa_sql::ParsedRegtypeName,
) -> Result<Option<i64>, uqa_sql::SQLError> {
    if oid
        .and_then(|oid| catalog.types.get(&oid))
        .is_some_and(|entry| !entry.is_defined)
    {
        return Err(uqa_sql::SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type \"{}\" is only a shell", parsed.names.join(".")),
        });
    }
    Ok(oid)
}

#[cfg(test)]
mod tests;
