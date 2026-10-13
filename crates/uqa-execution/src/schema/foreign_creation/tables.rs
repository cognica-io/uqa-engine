//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A foreign relation and its row type precede expression analysis and the server association.

use super::ForeignCreationContext;
use crate::catalog::{foreign::StoredForeignTable, security::BoundTableSecurity};
use crate::schema::sequences::ownership;
use crate::statement::prepared::invalidation::PreparedCatalogChange;
use uqa_core::RelationIdentity;
use uqa_sql::ast::{CreateForeignTable, RelationPersistence};
use uqa_sql::catalog::relation_oids::RelationOidKind;
use uqa_sql::SQLError;

/// Remove an incomplete declaration before the transaction refreshes its rolled-back catalog.
pub(super) struct ForeignDeclaration<'a> {
    context: &'a ForeignCreationContext<'a>,
    relation: RelationIdentity,
    object_id: [u8; 16],
    finished: bool,
}

impl ForeignDeclaration<'_> {
    pub(super) fn finish(mut self) {
        self.finished = true;
    }
}

impl Drop for ForeignDeclaration<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut tables = self.context.publication.tables_write();
        if tables
            .get(&self.relation)
            .is_none_or(|table| table.object_id != self.object_id)
        {
            return;
        }
        tables.remove(&self.relation);
        self.context
            .publication
            .security_write()
            .remove(&self.relation);
        drop(tables);
        self.context.changes.catalog_registry_changed();
    }
}

impl ForeignCreationContext<'_> {
    /// Publish only inside the caller's definition transaction. A failed expression, server lookup or validator rolls back the declaration and its row type together.
    pub(super) fn declare_foreign_relation(
        &self,
        relation: &RelationIdentity,
        owner: &crate::catalog::security::roles::locking::RoleBinding,
        statement: &mut CreateForeignTable,
        persistence: RelationPersistence,
    ) -> Result<(StoredForeignTable, ForeignDeclaration<'_>), SQLError> {
        let name = relation.qualified_name();
        self.creation.reserve_row_type_name(&name)?;
        let mut allocate = self
            .identities
            .allocator(crate::catalog::identity::allocate_catalog_object_id);
        let catalog_oids =
            allocate.allocate_relation_oids(RelationOidKind::ForeignTable, relation)?;
        let columns = uqa_sql::schema::table_creation::declaration::declared_row_columns(
            &mut statement.columns,
            &mut allocate,
        )?;
        let object_id = (self.allocate_identity)().map_err(|error| {
            SQLError::Internal(format!(
                "allocate foreign table `{name}` object identity: {error}"
            ))
        })?;
        let table = StoredForeignTable {
            name,
            persistence,
            object_id,
            catalog_oids: Some(catalog_oids),
            row_type_array_name: Some(crate::schema::types::arrays::reserve_array_name(
                &self.creation,
                &relation.schema,
                &relation.name,
            )?),
            server_name: statement.server_name.clone(),
            server_reference: None,
            columns,
            dropped_attributes: Vec::new(),
            checks: Vec::new(),
            options: statement.options.iter().cloned().collect(),
            option_order: Vec::new(),
        };
        let mut tables = self.publication.tables_write();
        let mut security = self.publication.security_write();
        if tables.contains_key(relation) || security.contains_key(relation) {
            return Err(SQLError::Internal(format!(
                "foreign table `{}` changed during creation",
                table.name
            )));
        }
        tables.insert(relation.clone(), table.clone());
        security.insert(
            relation.clone(),
            BoundTableSecurity::owner(owner.identity()),
        );
        drop(security);
        drop(tables);
        let declaration = ForeignDeclaration {
            context: self,
            relation: relation.clone(),
            object_id,
            finished: false,
        };
        self.changes.catalog_registry_changed();
        Ok((table, declaration))
    }

    pub(super) fn publish_foreign_relation(
        &self,
        relation: &RelationIdentity,
        owner: &crate::catalog::security::roles::locking::RoleBinding,
        table: StoredForeignTable,
    ) -> Result<(), SQLError> {
        let security = BoundTableSecurity::owner(owner.identity());
        let columns = table.columns.clone();
        let object_id = table.object_id;
        let catalog_oid = table.relation_oids().relation;
        let mut tables = self.publication.tables_write();
        if tables
            .get(relation)
            .is_none_or(|current| current.object_id != object_id)
        {
            return Err(SQLError::Internal(format!(
                "foreign table `{}` changed during creation",
                table.name
            )));
        }
        table
            .persist(self.catalog, relation, &security)
            .map_err(|error| {
                SQLError::Internal(format!("persist foreign table `{}`: {error}", table.name))
            })?;
        tables.insert(relation.clone(), table);
        drop(tables);
        ownership::attach_column_owners(
            &self.ownership,
            &relation.qualified_name(),
            object_id,
            &columns,
        )?;
        self.changes.catalog_registry_changed();
        self.changes
            .prepared_catalog_changed(PreparedCatalogChange::Relation(catalog_oid));
        Ok(())
    }
}
