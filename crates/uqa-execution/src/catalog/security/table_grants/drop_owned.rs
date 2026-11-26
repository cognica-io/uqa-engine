//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{authority, locking, prepared, TableGrantContext};
use crate::row_locks::{binding::acquire_relation, RelationLockMode};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::security::{
        drop_owned::RelationGrants,
        table_grants::{RelationAclChange, ResolvedTableGrantTarget},
    },
    SQLError,
};

impl TableGrantContext<'_> {
    pub(crate) fn remove_role_grants(
        &self,
        relation: RelationIdentity,
        kind: &'static str,
        oid: u32,
        names: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        let name = relation.qualified_name();
        acquire_relation(self.locks, &name, RelationLockMode::AccessShare, false)?.retain();
        for catalog in ["pg_catalog.pg_class", "pg_catalog.pg_attribute"] {
            acquire_relation(self.locks, catalog, RelationLockMode::RowExclusive, false)?.retain();
        }
        self.locks.refresh_after_wait()?;
        let target = ResolvedTableGrantTarget {
            requested: name.clone(),
            name,
            relation,
            kind,
            acl_columns: None,
        };
        let current_oid = if let Some(system) = super::targets::system_target(&target) {
            Some(system.oid())
        } else {
            self.bindings
                .relation_catalog_oid(&target.name)?
                .map(i64::from)
        };
        if current_oid != Some(i64::from(oid)) {
            return Ok(());
        }
        let (columns, security) = authority::security(self, &target)?;
        let changed = {
            let roles = self.roles.role_definitions();
            let change = RelationGrants {
                definitions: &roles,
                targets: names,
            };
            let before = security.resolve(&roles).map_err(SQLError::Internal)?;
            let (after, _) = change.apply_to(&target, &before)?;
            change.replaced_tuples(&before, &after)
        };
        for column in changed {
            let attribute = match &column {
                None => 0,
                Some(name) => {
                    columns
                        .iter()
                        .position(|column| column == name)
                        .ok_or_else(|| {
                            SQLError::Internal(format!("missing ACL attribute {name}"))
                        })? as u64
                        + 1
                }
            };
            locking::lock_tuple(self, &target, i64::from(oid), column.as_deref(), attribute)?;
        }
        self.writer.prepare_writer()?;
        let roles = self.roles.role_definitions();
        let change = RelationGrants {
            definitions: &roles,
            targets: names,
        };
        let (value, _) = prepared::prepare_acl_updates(self, &[target], &change)?;
        let (changed, _) = self.publish_acl_updates(value)?;
        if changed {
            self.changes.table_catalog_changed();
            self.changes.catalog_registry_changed();
        }
        Ok(())
    }
}
