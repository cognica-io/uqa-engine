//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog identities retain their canonical name while standalone data follows the exact public API names.

use super::{
    text, Catalog, Family, NativeRecordOwner, RelationIdentity, RelationKind, Result, SQLiteError,
};
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::VersionError;

impl Catalog {
    pub(in crate::catalog) fn rename_native_table(
        &self,
        from_name: &str,
        to_name: &str,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            let old_name = from.qualified_name();
            let new_name = to.qualified_name();
            let missing =
                || SQLiteError::StorageBackend(format!("table `{from_name}` does not exist"));
            let (owner, _) = snapshot.table_binding(&old_name)?.ok_or_else(missing)?;
            if !snapshot.contains_row(Family::Tables, owner, &[])? {
                return Err(missing());
            }
            if snapshot.relation_kind(to)?.is_some() {
                return Err(SQLiteError::StorageBackend(format!(
                    "relation `{new_name}` already exists"
                )));
            }
            let source = snapshot.table_owner(from_name)?;
            let target = snapshot.table_owner(to_name)?;
            let canonical_target = snapshot.table_owner(&new_name)?;
            if let (Some(source), Some(target)) = (source, target) {
                snapshot.check_table_data_merge(source, target)?;
            }
            snapshot.claim_relation(batch, to, RelationKind::Table)?;
            snapshot.rename_relation_acls(batch, from, to)?;
            snapshot.read_row(Family::Tables, owner, &[], |row| {
                let mut renamed = BudgetedVec::new(snapshot.control.memory());
                renamed.extend_from_slice(row).map_err(VersionError::from)?;
                renamed[0] = text(&to.schema);
                renamed[1] = text(&to.name);
                snapshot.put_row(batch, Family::Tables, owner, &renamed)
            })?;
            snapshot.delete_prefix(
                batch,
                Family::TableOwners,
                NativeRecordOwner::Database(snapshot.database),
                &[text(&old_name)],
            )?;
            if from_name != old_name {
                if snapshot.has_table_data(owner)? {
                    let retained = NativeRecordOwner::Object {
                        identity: snapshot.allocate_identity([0; 16])?,
                        generation: snapshot.allocate_identity([0; 16])?,
                    };
                    snapshot.transfer_table_data(batch, owner, retained, &old_name)?;
                    snapshot.put_table_binding(batch, &old_name, retained, false)?;
                } else {
                    // Internal guards follow their unchanged catalog owner even when there is no user data to retain under the old name.
                    snapshot.transfer_table_data(batch, owner, owner, &new_name)?;
                }
            }
            if let Some(source) = source {
                let target = if to_name == new_name {
                    owner
                } else {
                    snapshot.ensure_table_owner(to_name, batch)?
                };
                snapshot.transfer_table_data(batch, source, target, to_name)?;
                if from_name != old_name {
                    snapshot.delete_prefix(
                        batch,
                        Family::TableOwners,
                        NativeRecordOwner::Database(snapshot.database),
                        &[text(from_name)],
                    )?;
                }
            }
            if let Some(previous) = canonical_target {
                snapshot.transfer_table_data(batch, previous, owner, &new_name)?;
            }
            snapshot.rename_table_catalog_indexes(batch, from, to)?;
            snapshot.put_table_binding(batch, &new_name, owner, true)?;
            snapshot.release_relation(batch, from, RelationKind::Table)
        })
    }
}
