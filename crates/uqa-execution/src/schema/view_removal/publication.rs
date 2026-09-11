//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist view deletion while retaining the live publication guard.
use super::context::{ViewRemovalEvents, ViewRemovalPublication};
use crate::{
    catalog::view::ViewRegistryState, schema::publication::dependencies::CatalogPublicationChanges,
};
use uqa_core::RelationIdentity;
use uqa_sql::SQLError;

pub(super) fn drop_view_state_inner(
    registry: &dyn ViewRegistryState,
    publication: &dyn ViewRemovalPublication,
    events: &dyn ViewRemovalEvents,
    changes: &dyn CatalogPublicationChanges,
    name: &str,
) -> Result<(), SQLError> {
    let relation = RelationIdentity::from_legacy_name(name)
        .map_err(|err| SQLError::Internal(format!("invalid canonical view name: {err}")))?;
    events
        .drop_relation_events_inner(&relation)
        .map_err(|error| SQLError::Internal(format!("drop view rules: {error}")))?;
    let mut views = registry.views_write();
    let temporary = views
        .get(&relation)
        .is_some_and(|view| view.persistence == uqa_sql::ast::RelationPersistence::Temporary);
    let removed = if temporary {
        views.contains_key(&relation)
    } else {
        publication
            .drop_view(&relation)
            .map_err(|err| SQLError::Internal(format!("drop view `{name}`: {err}")))?
            .unwrap_or_else(|| views.contains_key(&relation))
    };
    if removed {
        views.remove(&relation);
    }
    drop(views);
    if removed {
        changes.catalog_registry_changed();
    }
    if removed {
        Ok(())
    } else {
        Err(SQLError::Internal(format!(
            "view `{name}` disappeared after dependency preflight"
        )))
    }
}

#[cfg(test)]
mod tests;
