//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute a sequence rename or schema move and publish every stored reference in order.
use super::dependencies::{
    rewrite_loaded_view_sequence_references, rewrite_sequence_schema_dependencies,
    ViewSequenceRewriteContext,
};
use crate::schema::{
    events::EventCatalogContext,
    namespaces::relations::RelationCreationContext,
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
    pub creation: RelationCreationContext<'a>,
    pub schemas: SchemaDependencyPublicationContext<'a>,
    pub views: ViewSequenceRewriteContext<'a>,
    pub state: &'a dyn SequenceStateRename,
    pub catalog: &'a dyn SequenceRenameCatalog,
    pub events: EventCatalogContext<'a>,
    pub changes: &'a dyn CatalogPublicationChanges,
}
pub fn alter_sequence_lifecycle(
    context: &SequenceLifecycleContext<'_>,
    _source_name: &str,
    source: &RelationIdentity,
    persistence: RelationPersistence,
    alter: &AlterSequence,
) -> Result<(), SQLError> {
    analysis::validate_sequence_lifecycle_shape(alter)?;
    context.views.views.synchronize_catalog().map_err(|error| {
        uqa_sql::catalog::errors::storage_error("sequence relocation catalog", &error)
    })?;
    let mut target =
        analysis::sequence_lifecycle_target(context.analysis, source, &alter.lifecycle)?;
    if matches!(
        alter.lifecycle,
        uqa_sql::ast::SequenceLifecycle::SetSchema { .. }
    ) {
        target = context.creation.relocation_target(&target)?;
    }
    if !analysis::validate_sequence_lifecycle_target(
        context.analysis,
        source,
        &target,
        persistence,
        &alter.lifecycle,
    )? {
        return Ok(());
    }
    let target_name = target.qualified_name();
    context.creation.reserve_name(&target_name)?;
    publish_relocation(context, source, &target, persistence)
}

/// Publish a destination whose relation and namespace locks the caller already retained. This also serves sequences moving with their owning table, whose ownership must not be cleared to pass the standalone ALTER SEQUENCE check.
pub(crate) fn publish_relocation(
    context: &SequenceLifecycleContext<'_>,
    source: &RelationIdentity,
    target: &RelationIdentity,
    persistence: RelationPersistence,
) -> Result<(), SQLError> {
    let source_name = source.qualified_name();
    let target_name = target.qualified_name();
    rewrite_sequence_schema_dependencies(&context.schemas, source, &target_name).map_err(
        |error| {
            SQLError::Internal(format!(
                "rewrite table dependencies for sequence `{source_name}`: {error}"
            ))
        },
    )?;
    rewrite_loaded_view_sequence_references(&context.views, source, &target_name).map_err(
        |error| {
            SQLError::Internal(format!(
                "rewrite view dependencies for sequence `{source_name}`: {error}"
            ))
        },
    )?;
    if persistence != RelationPersistence::Temporary
        && context.catalog.has_catalog()
        && !context
            .catalog
            .rename_sequence_row(&source_name, &target_name)
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
    context.state.move_state(source, target)?;
    crate::schema::events::rename_relation_events(&context.events, source, target).map_err(
        |error| {
            SQLError::Internal(format!(
                "rewrite rule dependencies for sequence `{source_name}`: {error}"
            ))
        },
    )?;
    context.changes.catalog_registry_changed();
    Ok(())
}
