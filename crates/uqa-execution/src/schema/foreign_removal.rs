//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove a foreign table's own state and publish the removal inside the caller's transaction.
use crate::catalog::foreign::lookup::ForeignLookupContext;
use crate::schema::{
    events::context::EventLifecycleContext, foreign_table_alteration::ForeignTableAlterPublication,
    publication::dependencies::CatalogPublicationChanges,
};
use uqa_core::RelationIdentity;
use uqa_storage::CatalogFacade;
pub struct ForeignTableRemovalContext<'a> {
    pub lookup: ForeignLookupContext<'a>,
    pub publication: &'a dyn ForeignTableAlterPublication,
    pub catalog: Option<&'a dyn CatalogFacade>,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub events: EventLifecycleContext<'a>,
}
impl ForeignTableRemovalContext<'_> {
    pub fn drop_foreign_table_inner(&self, name: &str) -> Result<bool, String> {
        self.lookup
            .state
            .synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let Some(name) = self
            .lookup
            .resolve_foreign_table_name(name)
            .map_err(|err| format!("resolve foreign table: {err}"))?
        else {
            return Ok(false);
        };
        let relation = RelationIdentity::from_legacy_name(&name)?;
        let persistence = self
            .lookup
            .registry
            .tables()
            .get(&relation)
            .map(|table| table.persistence)
            .ok_or_else(|| format!("Foreign table `{name}` disappeared before drop"))?;
        if !self.lookup.registry.security().contains_key(&relation) {
            return Err(format!(
                "Foreign table `{name}` has no loaded security metadata"
            ));
        }
        self.events
            .drop_relation_events_inner(&relation)
            .map_err(|error| format!("drop foreign table `{name}` events: {error}"))?;
        if let Some(catalog) = self
            .catalog
            .filter(|_| persistence != uqa_sql::ast::RelationPersistence::Temporary)
        {
            catalog
                .drop_foreign_table(&relation)
                .map_err(|err| format!("drop foreign table `{name}`: {err}"))?;
        }
        self.publication.memory_tables_write().remove(&relation);
        let mut tables = self.publication.tables_write();
        let mut table_security = self.publication.security_write();
        let removed = tables.remove(&relation);
        table_security.remove(&relation);
        drop(table_security);
        drop(tables);
        if let Some(table) = &removed {
            self.changes.catalog_registry_changed();
            self.changes.prepared_catalog_changed(
                crate::statement::prepared::invalidation::PreparedCatalogChange::Relation(
                    table.relation_oids().relation,
                ),
            );
        }
        Ok(removed.is_some())
    }
}

/// Remove a session's temporary foreign definitions, security and in-memory FDW rows.
pub fn discard_temporary_foreign_tables(
    publication: &dyn ForeignTableAlterPublication,
    schema: &str,
) {
    publication
        .tables_write()
        .retain(|relation, _| relation.schema != schema);
    publication
        .security_write()
        .retain(|relation, _| relation.schema != schema);
    publication
        .memory_tables_write()
        .retain(|relation, _| relation.schema != schema);
}
