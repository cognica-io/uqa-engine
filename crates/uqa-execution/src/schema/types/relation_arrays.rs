//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist generated array names in their relation definitions, using the existing catalog transaction.

pub mod restoration;

use std::borrow::Cow;

use super::super::{
    foreign_table_alteration::{ForeignTableAlterCatalog, ForeignTableAlterPublication},
    namespaces::relations::RelationCreationContext,
    publication::{dependencies::CatalogPublicationChanges, TableSchemaCatalog},
};
use crate::catalog::{services::CatalogSnapshotSource, view::ViewPublication, CatalogReadView};
use crate::row_locks::{
    binding::{lock_any_relation_identity, RelationDefinitionSession, RelationLockCatalog},
    shared_objects::SharedCatalogLock,
    RelationLockMode,
};
use uqa_core::RelationIdentity;
use uqa_sql::{catalog::array_type_names::array_type_name, SQLError};

#[derive(Clone, Copy)]
pub struct RelationArrayContext<'a> {
    pub catalog: &'a dyn CatalogSnapshotSource,
    pub tables: &'a dyn TableSchemaCatalog,
    pub views: &'a dyn ViewPublication,
    pub foreign: &'a dyn ForeignTableAlterCatalog,
    pub foreign_publication: &'a dyn ForeignTableAlterPublication,
    pub storage: Option<&'a dyn uqa_storage::CatalogFacade>,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub relations: &'a dyn RelationLockCatalog,
    pub locks: &'a dyn RelationDefinitionSession,
}

#[derive(Clone, Copy)]
enum HolderKind {
    Table,
    View,
    Foreign,
}

struct Holder {
    relation: RelationIdentity,
    object_id: [u8; 16],
    kind: HolderKind,
    array_oid: u32,
}

/// The only relation metadata needed for generated array name inquiries.
pub struct RelationArrayMetadata<'a> {
    pub array_oid: Option<u32>,
    pub recorded_name: Option<Cow<'a, str>>,
}

fn matches(
    relation: &RelationIdentity,
    oid: Option<u32>,
    recorded: Option<&str>,
    requested: &RelationIdentity,
) -> bool {
    oid.is_some()
        && relation.schema == requested.schema
        && recorded.map_or_else(
            || array_type_name(&relation.name, 0) == requested.name,
            |name| name == requested.name,
        )
}

fn holder(catalog: &CatalogReadView, requested: &RelationIdentity) -> Option<Holder> {
    let snapshot = catalog.snapshot();
    for (relation, table) in &snapshot.tables {
        if matches(
            relation,
            table.catalog_oids.array_type,
            table.row_type_array_name.as_deref(),
            requested,
        ) {
            return Some(Holder {
                relation: relation.clone(),
                object_id: table.object_id,
                kind: HolderKind::Table,
                array_oid: table.catalog_oids.array_type?,
            });
        }
    }
    for (relation, view) in snapshot.definitions.views.iter() {
        if matches(
            relation,
            view.relation_oids().array_type,
            view.row_type_array_name.as_deref(),
            requested,
        ) {
            return Some(Holder {
                relation: relation.clone(),
                object_id: view.object_id,
                kind: HolderKind::View,
                array_oid: view.relation_oids().array_type?,
            });
        }
    }
    for (relation, foreign) in snapshot.definitions.foreign_tables.iter() {
        if matches(
            relation,
            foreign.relation_oids().array_type,
            foreign.row_type_array_name.as_deref(),
            requested,
        ) {
            return Some(Holder {
                relation: relation.clone(),
                object_id: foreign.object_id,
                kind: HolderKind::Foreign,
                array_oid: foreign.relation_oids().array_type?,
            });
        }
    }
    None
}

pub fn type_name_exists<'a>(
    arrays: impl IntoIterator<Item = (&'a RelationIdentity, RelationArrayMetadata<'a>)>,
    requested: &RelationIdentity,
) -> bool {
    arrays.into_iter().any(|(relation, array)| {
        matches(
            relation,
            array.array_oid,
            array.recorded_name.as_deref(),
            requested,
        )
    })
}

pub fn displace(
    context: RelationArrayContext<'_>,
    creation: &RelationCreationContext<'_>,
    requested: &RelationIdentity,
) -> Result<bool, SQLError> {
    let Some(initial) = holder(&context.catalog.current_catalog_snapshot(), requested) else {
        return Ok(false);
    };
    let Some(_) = lock_any_relation_identity(
        context.relations,
        context.locks,
        initial.relation.qualified_name(),
        initial.object_id,
        RelationLockMode::AccessExclusive,
    )?
    else {
        return Ok(false);
    };
    let guard = creation.locks.acquire_shared_catalog(
        SharedCatalogLock::Object {
            class_id: super::super::namespaces::type_names::TYPE_CATALOG_CLASS_ID,
            oid: initial.array_oid,
        },
        RelationLockMode::AccessExclusive,
    )?;
    creation.locks.refresh_shared_catalog()?;
    let Some(current) = holder(&context.catalog.current_catalog_snapshot(), requested) else {
        return Ok(false);
    };
    if current.object_id != initial.object_id || current.array_oid != initial.array_oid {
        return Err(changed());
    }
    let name = super::arrays::reserve_array_name(creation, &requested.schema, &requested.name)?;
    context.locks.prepare_definition_write()?;
    let Some(current) = holder(&context.catalog.current_catalog_snapshot(), requested) else {
        return Err(changed());
    };
    if current.object_id != initial.object_id || current.array_oid != initial.array_oid {
        return Err(changed());
    }
    publish(context, &current, name)?;
    guard.retain();
    Ok(true)
}

fn changed() -> SQLError {
    SQLError::Routine {
        sqlstate: "40001".into(),
        message: "relation changed while reserving its array type name".into(),
    }
}

fn storage(error: uqa_storage::StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error("relation array type", &error)
}

fn publish(
    context: RelationArrayContext<'_>,
    holder: &Holder,
    name: String,
) -> Result<(), SQLError> {
    match holder.kind {
        HolderKind::Table => {
            let state = context
                .tables
                .table_state(&holder.relation.qualified_name())
                .map_err(storage)?
                .ok_or_else(changed)?;
            if state.object_id() != holder.object_id {
                return Err(changed());
            }
            let columns = state.columns();
            let mut constraints = state.constraints();
            constraints.row_type_array_name = Some(name);
            state
                .persist_candidate(&columns, &constraints)
                .map_err(storage)?;
            state.publish_constraints(columns, constraints);
            context.changes.table_catalog_changed();
        }
        HolderKind::View => {
            let mut views = context.views.views_write();
            let current = views.get(&holder.relation).ok_or_else(changed)?;
            if current.object_id != holder.object_id {
                return Err(changed());
            }
            let mut view = current.clone();
            view.row_type_array_name = Some(name);
            if view.persistence != uqa_sql::ast::RelationPersistence::Temporary {
                context
                    .views
                    .save_view(
                        &crate::catalog::view::catalog_view_row(&holder.relation, &view)
                            .map_err(|error| SQLError::Internal(error.to_string()))?,
                    )
                    .map_err(storage)?;
            }
            views.insert(holder.relation.clone(), view);
            drop(views);
            context.changes.catalog_registry_changed();
        }
        HolderKind::Foreign => {
            let security = context
                .foreign
                .security(&holder.relation)
                .ok_or_else(changed)?;
            let mut tables = context.foreign_publication.tables_write();
            let current = tables.get(&holder.relation).ok_or_else(changed)?;
            if current.object_id != holder.object_id {
                return Err(changed());
            }
            let mut table = current.clone();
            table.row_type_array_name = Some(name);
            table
                .persist(context.storage, &holder.relation, &security)
                .map_err(storage)?;
            tables.insert(holder.relation.clone(), table);
            drop(tables);
            context.changes.catalog_registry_changed();
        }
    }
    Ok(())
}

/// Rename generates a fresh array name; a schema move preserves the recorded name and rejects an occupied destination.
pub fn rename(
    creation: &RelationCreationContext<'_>,
    from: &RelationIdentity,
    to: &RelationIdentity,
    recorded: Option<&str>,
    array_oid: Option<u32>,
) -> Result<Option<String>, SQLError> {
    let displaced_own_array = from.schema == to.schema
        && array_oid.is_some()
        && recorded.map_or_else(
            || array_type_name(&from.name, 0) == to.name,
            |name| name == to.name,
        );
    if from.schema == to.schema {
        creation.reserve_row_type_name(&to.qualified_name())?;
    } else {
        creation.reserve_row_type_relocation(&to.qualified_name())?;
    }
    let Some(array_oid) = array_oid else {
        return Ok(None);
    };
    if displaced_own_array {
        // RenameTypeInternal does not rename the companion a second time when the row type has just taken that companion's old name.
        return creation
            .runtime
            .relation_array_name(from, array_oid)
            .map(Some)
            .ok_or_else(changed);
    }
    if from.schema == to.schema {
        return super::arrays::reserve_array_name(creation, &to.schema, &to.name).map(Some);
    }
    let name = recorded.map_or_else(|| array_type_name(&from.name, 0), str::to_owned);
    let target = RelationIdentity::new(&to.schema, &name);
    if creation.type_name_in_use(&target) {
        return Err(SQLError::Routine {
            sqlstate: "42710".into(),
            message: format!("type \"{name}\" already exists in schema \"{}\"", to.schema),
        });
    }
    creation.reserve_type_name(&target.qualified_name())?;
    Ok(Some(name))
}

/// Read the current name of one relation's fixed array identity after a catalog wait or displacement.
pub fn current_array_name<'a>(
    candidates: impl IntoIterator<Item = RelationArrayMetadata<'a>>,
    oid: u32,
) -> Option<String> {
    candidates.into_iter().find_map(|array| {
        (array.array_oid == Some(oid))
            .then_some(array.recorded_name)
            .flatten()
            .map(Cow::into_owned)
    })
}

#[cfg(test)]
mod tests;
