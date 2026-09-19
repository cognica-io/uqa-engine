//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare relation and attribute tuple replacements without rewriting relation definitions.

use super::{context::TableGrantContext, updates::TablePrivilegeUpdate};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::RelationPersistence,
    catalog::security::{
        table_grants::{ForeignTablePrivilegeUpdate, TableGrantApplication, ViewPrivilegeUpdate},
        BoundTableSecurity,
    },
    SQLError,
};
use uqa_storage::{catalog::relation_acl::RelationAclTuple, StorageBackendResult};

pub(super) struct RelationPrivilegeUpdate {
    relation: RelationIdentity,
    column: Option<String>,
    entry: RelationAclTuple,
}

impl RelationPrivilegeUpdate {
    pub(super) fn persist(
        &self,
        acls: &dyn super::context::TableGrantPersistence,
    ) -> StorageBackendResult<()> {
        acls.persist_relation_acl(&self.relation, self.column.as_deref(), &self.entry)
    }
}

pub(super) fn prepare(
    context: &TableGrantContext<'_>,
    targets: &[uqa_sql::catalog::security::table_grants::ResolvedTableGrantTarget],
    application: &TableGrantApplication<'_>,
    tables: &mut [TablePrivilegeUpdate<'_>],
    views: &mut [ViewPrivilegeUpdate],
    foreign: &mut [ForeignTablePrivilegeUpdate],
) -> Result<Vec<RelationPrivilegeUpdate>, SQLError> {
    let mut updates = Vec::new();
    let mut stamp = |relation: &RelationIdentity,
                     before: &BoundTableSecurity,
                     after: &mut BoundTableSecurity,
                     persistence| {
        let original = before
            .resolve(application.roles)
            .map_err(SQLError::Internal)?;
        let next = after
            .resolve(application.roles)
            .map_err(SQLError::Internal)?;
        let mut row = after.row();
        row.acl_revisions.clone_from(&before.acl_revisions);
        let target = targets
            .iter()
            .find(|target| target.relation == *relation)
            .ok_or_else(|| SQLError::Internal("ACL tuple has no bound target".into()))?;
        for column in application.replaced_tuples(&original, &next) {
            if !target.includes_acl_tuple(column.as_deref()) {
                continue;
            }
            let acl = column.as_ref().map_or_else(
                || row.acl.clone(),
                |column| Some(row.column_acls.get(column).cloned().unwrap_or_default()),
            );
            let entry = RelationAclTuple::new(acl)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            entry
                .apply(column.as_deref(), &mut row)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            if persistence != RelationPersistence::Temporary {
                updates.push(RelationPrivilegeUpdate {
                    relation: relation.clone(),
                    column,
                    entry,
                });
            }
        }
        *after = BoundTableSecurity::from_row(row);
        Ok::<_, SQLError>(())
    };
    for (name, table, security) in tables {
        let relation = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
        stamp(&relation, &table.security(), security, table.persistence())?;
    }
    let existing_views = context.registry.views();
    for (relation, view) in views {
        let before = existing_views
            .get(relation)
            .ok_or_else(|| SQLError::Internal("GRANT view disappeared".into()))?;
        let persistence = view.persistence;
        stamp(relation, &before.security, &mut view.security, persistence)?;
    }
    let existing_foreign = context.registry.foreign_security();
    for (relation, security) in foreign {
        let before = existing_foreign
            .get(relation)
            .ok_or_else(|| SQLError::Internal("GRANT foreign table disappeared".into()))?;
        stamp(relation, before, security, RelationPersistence::Permanent)?;
    }
    Ok(updates)
}
