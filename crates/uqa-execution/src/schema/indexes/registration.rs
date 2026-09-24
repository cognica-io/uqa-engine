//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserve a stored index address and retain the indexed table across catalog refresh.

use crate::catalog::{identity::CatalogIdentityReservationContext, index::index_definition};
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::index::{IndexCatalogIdentity, IndexDefinition},
    schema::constraint_metadata::{CatalogObjectAllocator, CatalogOidClass},
    SQLError,
};

pub fn prepare(
    context: CatalogIdentityReservationContext<'_>,
    relation: &RelationIdentity,
    table: &RelationIdentity,
    definition: &IndexDefinition,
) -> Result<IndexDefinition, SQLError> {
    let catalog = context.catalog.current_catalog_snapshot();
    let table_object_id = catalog
        .snapshot()
        .tables
        .get(table)
        .ok_or_else(|| SQLError::UnknownTable(table.qualified_name()))?
        .object_id;
    let previous = catalog
        .snapshot()
        .definitions
        .catalog_indexes
        .get(relation)
        .map(index_definition)
        .transpose()
        .map_err(|error| uqa_sql::catalog::errors::storage_error("index identity", &error))?;
    let mut definition = definition.clone();
    let previous_identity = previous
        .as_ref()
        .and_then(|definition| definition.catalog.as_ref());
    if let (Some(supplied), Some(previous)) = (&definition.catalog, previous_identity) {
        if supplied != previous {
            return Err(SQLError::Internal(
                "cannot replace a retained physical index identity".into(),
            ));
        }
    }
    let mut allocator = context.allocator(crate::catalog::identity::allocate_catalog_object_id);
    let identity = if let Some(identity) = definition.catalog.take().or_else(|| {
        previous_identity
            .filter(|identity| identity.table_object_id == table_object_id)
            .cloned()
    }) {
        identity.validate(table_object_id).map_err(metadata_error)?;
        allocator
            .include_catalog_identity(relation, CatalogOidClass::Relation, identity.identity)
            .map_err(metadata_error)?;
        identity
    } else {
        IndexCatalogIdentity::allocate(table_object_id, &mut allocator).map_err(metadata_error)?
    };
    let current = context.catalog.current_catalog_snapshot();
    let current_index = current
        .snapshot()
        .definitions
        .catalog_indexes
        .get(relation)
        .map(index_definition)
        .transpose()
        .map_err(|error| uqa_sql::catalog::errors::storage_error("index identity", &error))?;
    if current_index
        .as_ref()
        .and_then(|definition| definition.catalog.as_ref())
        != previous_identity
    {
        return Err(SQLError::Routine {
            sqlstate: "40001".into(),
            message: format!(
                "index `{}` was replaced during address reservation",
                relation.qualified_name()
            ),
        });
    }
    if current
        .snapshot()
        .tables
        .get(table)
        .map(|table| table.object_id)
        != Some(table_object_id)
    {
        return Err(SQLError::Routine {
            sqlstate: "40001".into(),
            message: format!(
                "indexed table `{}` was replaced during address reservation",
                table.qualified_name()
            ),
        });
    }
    definition.catalog = Some(identity);
    Ok(definition)
}

fn metadata_error(
    error: uqa_sql::schema::constraint_metadata::ConstraintMetadataError,
) -> SQLError {
    match error {
        uqa_sql::schema::constraint_metadata::ConstraintMetadataError::Execution(error) => *error,
        uqa_sql::schema::constraint_metadata::ConstraintMetadataError::Invalid(message) => {
            SQLError::Internal(message)
        }
    }
}
