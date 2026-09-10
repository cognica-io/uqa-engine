//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute a sequence rename or schema move and publish every stored reference in order.
use super::{
    creation::SequenceCreationNamespace,
    dependencies::{
        rewrite_sequence_schema_dependencies, rewrite_view_sequence_references,
        ViewSequenceRewriteContext,
    },
};
use crate::schema::{
    events::EventCatalogContext,
    publication::dependencies::{CatalogPublicationChanges, SchemaDependencyPublicationContext},
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{AlterSequence, RelationPersistence},
    schema::sequences::lifecycle as analysis,
    SQLError,
};
use uqa_storage::StorageBackendResult;

pub trait SequenceStateRename {
    fn move_state(
        &self,
        source: &RelationIdentity,
        target: &RelationIdentity,
    ) -> Result<(), SQLError>;
}
pub trait SequenceRenameCatalog {
    fn has_catalog(&self) -> bool;
    fn rename_sequence_row(&self, source: &str, target: &str) -> StorageBackendResult<bool>;
}
pub struct SequenceLifecycleContext<'a> {
    pub analysis: &'a dyn analysis::SequenceLifecycleCatalog,
    pub schemas: SchemaDependencyPublicationContext<'a>,
    pub views: ViewSequenceRewriteContext<'a>,
    pub state: &'a dyn SequenceStateRename,
    pub catalog: &'a dyn SequenceRenameCatalog,
    pub refresh: &'a dyn SequenceCreationNamespace,
    pub events: EventCatalogContext<'a>,
    pub changes: &'a dyn CatalogPublicationChanges,
}
pub fn alter_sequence_lifecycle(
    context: &SequenceLifecycleContext<'_>,
    source_name: &str,
    source: &RelationIdentity,
    persistence: RelationPersistence,
    alter: &AlterSequence,
) -> Result<(), SQLError> {
    analysis::validate_sequence_lifecycle_shape(alter)?;
    let Some(target) = analysis::sequence_lifecycle_target(
        context.analysis,
        source,
        persistence,
        &alter.lifecycle,
    )?
    else {
        return Ok(());
    };
    let target_name = target.qualified_name();
    rewrite_sequence_schema_dependencies(&context.schemas, source, &target_name).map_err(
        |error| {
            SQLError::Internal(format!(
                "rewrite table dependencies for sequence `{source_name}`: {error}"
            ))
        },
    )?;
    rewrite_view_sequence_references(&context.views, source, &target_name).map_err(|error| {
        SQLError::Internal(format!(
            "rewrite view dependencies for sequence `{source_name}`: {error}"
        ))
    })?;
    if persistence == RelationPersistence::Temporary {
        context.state.move_state(source, &target)?;
    } else if context.catalog.has_catalog() {
        if !context
            .catalog
            .rename_sequence_row(source_name, &target_name)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "persist sequence rename `{source_name}` to `{target_name}`: {error}"
                ))
            })?
        {
            return Err(SQLError::Internal(format!(
                "sequence `{source_name}` disappeared during rename"
            )));
        }
        context.refresh.refresh_sequences().map_err(|error| {
            SQLError::Internal(format!(
                "refresh sequence `{target_name}` after rename: {error}"
            ))
        })?;
    } else {
        context.state.move_state(source, &target)?;
    }
    crate::schema::events::rename_relation_events(&context.events, source, &target).map_err(
        |error| {
            SQLError::Internal(format!(
                "rewrite rule dependencies for sequence `{source_name}`: {error}"
            ))
        },
    )?;
    context.changes.catalog_registry_changed();
    Ok(())
}
