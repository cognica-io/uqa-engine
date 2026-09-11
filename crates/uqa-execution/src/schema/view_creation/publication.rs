//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist view definitions with the original registry guard and publication boundaries.
use super::super::publication::dependencies::CatalogPublicationChanges;
use crate::catalog::view::{catalog_view_row, StoredView, ViewPublication};
use uqa_core::RelationIdentity;
use uqa_sql::{ast::RelationPersistence, SQLError};

pub(super) fn publish_regular_view(
    publication: &dyn ViewPublication,
    changes: &dyn CatalogPublicationChanges,
    relation: RelationIdentity,
    view: StoredView,
    name: &str,
) -> Result<(), SQLError> {
    let mut views = publication.views_write();
    if view.persistence != RelationPersistence::Temporary && publication.has_catalog() {
        publication
            .save_view(
                &catalog_view_row(&relation, &view).map_err(|error| {
                    SQLError::Internal(format!("serialize view `{name}`: {error}"))
                })?,
            )
            .map_err(|error| SQLError::Internal(format!("persist view `{name}`: {error}")))?;
    }
    views.insert(relation, view);
    drop(views);
    changes.catalog_registry_changed();
    Ok(())
}
pub(super) fn publish_materialized_view(
    publication: &dyn ViewPublication,
    changes: &dyn CatalogPublicationChanges,
    relation: RelationIdentity,
    view: StoredView,
    name: &str,
) -> Result<(), SQLError> {
    if publication.has_catalog() {
        publication
            .save_view(&catalog_view_row(&relation, &view).map_err(|error| {
                SQLError::Internal(format!("serialize materialized view `{name}`: {error}"))
            })?)
            .map_err(|error| {
                SQLError::Internal(format!("persist materialized view `{name}`: {error}"))
            })?;
    }
    publication.views_write().insert(relation, view);
    changes.catalog_registry_changed();
    Ok(())
}

#[cfg(test)]
mod tests;
