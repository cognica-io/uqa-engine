//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{authority, locking, prepared, TableGrantContext};
use crate::row_locks::{binding::acquire_relation, RelationLockMode};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::security::{drop_owned::RelationGrants, table_grants::ResolvedTableGrantTarget},
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
        let (columns, _) = authority::security(self, &target)?;
        let attributes: BTreeMap<_, _> = columns
            .into_iter()
            .enumerate()
            .map(|(index, name)| (name, index as u64 + 1))
            .collect();
        self.writer.prepare_writer()?;
        let mut locked = BTreeSet::new();
        loop {
            let (candidate, _) = {
                let roles = self.roles.role_definitions();
                let change = RelationGrants {
                    definitions: &roles,
                    targets: names,
                };
                prepared::prepare_acl_updates(self, std::slice::from_ref(&target), &change)?
            };
            let mut missing = BTreeMap::new();
            for column in candidate
                .acl_updates
                .iter()
                .map(|update| &update.column)
                .chain(candidate.system_updates.iter().map(|update| &update.column))
            {
                let attribute = match column {
                    None => 0,
                    Some(name) => *attributes.get(name).ok_or_else(|| {
                        SQLError::Internal(format!("missing ACL attribute {name}"))
                    })?,
                };
                if !locked.contains(&attribute) {
                    missing.insert(attribute, column.clone());
                }
            }
            if missing.is_empty() {
                let (changed, _) = self.publish_acl_updates(candidate)?;
                if changed {
                    self.changes.table_catalog_changed();
                    self.changes.catalog_registry_changed();
                }
                return Ok(());
            }
            // Waiting can refresh other ACL tuples and change which grants cascade.
            drop(candidate);
            for (attribute, column) in missing {
                locking::lock_tuple(self, &target, i64::from(oid), column.as_deref(), attribute)?;
                locked.insert(attribute);
            }
        }
    }
}
