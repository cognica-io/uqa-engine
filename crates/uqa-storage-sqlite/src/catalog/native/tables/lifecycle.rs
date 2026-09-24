//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Definition-only deletion retains standalone data ownership; full deletion retires every owned row.

use super::{text, Catalog, Family, NativeRecordOwner, RelationIdentity, RelationKind, Result};

impl Catalog {
    pub(in crate::catalog) fn drop_native_table(
        &self,
        relation: &RelationIdentity,
        data: bool,
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            snapshot.clear_relation_acls(batch, relation)?;
            snapshot.release_relation(batch, relation, RelationKind::Table)?;
            snapshot.drop_table_catalog_indexes(batch, relation)?;
            let name = relation.qualified_name();
            if let Some((owner, catalog)) = snapshot.table_binding(&name)? {
                snapshot.reset_occurrence_rows(batch, owner)?;
                snapshot.delete_prefix(batch, Family::Tables, owner, &[])?;
                if !data && catalog {
                    snapshot.put_table_binding(batch, &name, owner, false)?;
                }
            }
            if data {
                for name in relation.canonical_and_legacy_public_names() {
                    if let Some(owner) = snapshot.table_owner(&name)? {
                        snapshot.clear_table_data(batch, owner, true)?;
                        snapshot.delete_prefix(
                            batch,
                            Family::TableOwners,
                            NativeRecordOwner::Database(snapshot.database),
                            &[text(&name)],
                        )?;
                    }
                }
            }
            Ok(())
        })
    }

    pub(in crate::catalog) fn purge_native_table_data(
        &self,
        relation: &RelationIdentity,
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            for name in relation.canonical_and_legacy_public_names() {
                if let Some(owner) = snapshot.table_owner(&name)? {
                    snapshot.clear_table_data(batch, owner, false)?;
                }
            }
            Ok(())
        })
    }
}
