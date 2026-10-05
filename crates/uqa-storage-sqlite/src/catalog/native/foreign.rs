//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign definitions and security share the native catalog transaction.

use super::{
    optional_text, string, text, Catalog, Family, NativeRecordOwner, RelationRecord, Result,
};
use crate::catalog::{ForeignServerRow, ForeignTableRow, RelationIdentity};
use crate::SQLiteError;
use std::collections::BTreeMap;

impl Catalog {
    pub(in crate::catalog) fn load_native_foreign_server_rows(
        &self,
    ) -> Result<Option<Vec<ForeignServerRow>>> {
        self.read_native(|snapshot| {
            let owner = NativeRecordOwner::Database(snapshot.database);
            let mut servers = BTreeMap::new();
            snapshot.visit_rows(Family::ForeignServers, Some(owner), &[], |row| {
                let name = string(row[0])?;
                servers.insert(
                    name.clone(),
                    ForeignServerRow {
                        name,
                        fdw_type: string(row[1])?,
                        options_json: string(row[2])?,
                        metadata_json: None,
                    },
                );
                Ok(())
            })?;
            snapshot.visit_rows(Family::ForeignServerMetadata, Some(owner), &[], |row| {
                let name = string(row[0])?;
                let server = servers.get_mut(&name).ok_or_else(|| {
                    SQLiteError::StorageBackend(format!(
                        "foreign server metadata references missing server `{name}`"
                    ))
                })?;
                server.metadata_json = Some(string(row[1])?);
                Ok(())
            })?;
            Ok(servers.into_values().collect())
        })
    }

    pub(in crate::catalog) fn save_native_foreign_server_row(
        &self,
        row: &ForeignServerRow,
    ) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            let owner = NativeRecordOwner::Database(snapshot.database);
            snapshot.put_row(
                batch,
                Family::ForeignServers,
                owner,
                &[
                    text(&row.name),
                    text(&row.fdw_type),
                    text(&row.options_json),
                ],
            )?;
            match &row.metadata_json {
                Some(metadata) => snapshot.put_row(
                    batch,
                    Family::ForeignServerMetadata,
                    owner,
                    &[text(&row.name), text(metadata)],
                ),
                None => snapshot.delete_prefix(
                    batch,
                    Family::ForeignServerMetadata,
                    owner,
                    &[text(&row.name)],
                ),
            }
        })
    }

    pub(in crate::catalog) fn drop_native_foreign_server(&self, name: &str) -> Result<Option<()>> {
        self.conn.with_native_write(|snapshot, batch| {
            let owner = NativeRecordOwner::Database(snapshot.database);
            snapshot.delete_prefix(batch, Family::ForeignServerMetadata, owner, &[text(name)])?;
            snapshot.delete_prefix(batch, Family::ForeignServers, owner, &[text(name)])
        })
    }

    pub(in crate::catalog) fn save_native_foreign_table(
        &self,
        row: &ForeignTableRow,
    ) -> Result<Option<()>> {
        if !self.conn.is_native_record_session() {
            return Ok(None);
        }
        let (security_owner, acl, columns) =
            super::super::role_security::encode_relation(&row.security)?;
        self.save_native_relation(
            RelationRecord::ForeignTable,
            &row.relation,
            &[
                text(&row.relation.schema),
                text(&row.relation.name),
                text("foreign_table"),
                text(&row.server_name),
                text(&row.columns_json),
                text(&row.options_json),
                (&security_owner).into(),
                optional_text(acl.as_deref()),
                text(&columns),
            ],
        )
    }

    pub(in crate::catalog) fn update_native_foreign_security(
        &self,
        relation: &RelationIdentity,
        security: &uqa_storage::RelationSecurityRow,
    ) -> Result<Option<bool>> {
        self.conn.with_native_write(|snapshot, batch| {
            snapshot.clear_relation_acls(batch, relation)?;
            let (security_owner, acl, columns) =
                super::super::role_security::encode_relation(security)?;
            let owner = NativeRecordOwner::Database(snapshot.database);
            Ok(snapshot
                .read_row(
                    Family::ForeignTables,
                    owner,
                    &[text(&relation.schema), text(&relation.name)],
                    |row| {
                        snapshot.put_row(
                            batch,
                            Family::ForeignTables,
                            owner,
                            &[
                                row[0],
                                row[1],
                                row[2],
                                row[3],
                                row[4],
                                row[5],
                                (&security_owner).into(),
                                optional_text(acl.as_deref()),
                                text(&columns),
                            ],
                        )
                    },
                )?
                .is_some())
        })
    }

    pub(in crate::catalog) fn load_native_foreign_tables(
        &self,
    ) -> Result<Option<Vec<ForeignTableRow>>> {
        self.read_native(|snapshot| {
            let mut tables = Vec::new();
            snapshot.visit_rows(
                Family::ForeignTables,
                Some(NativeRecordOwner::Database(snapshot.database)),
                &[],
                |row| {
                    tables.push(ForeignTableRow {
                        relation: RelationIdentity::new(string(row[0])?, string(row[1])?),
                        server_name: string(row[3])?,
                        columns_json: string(row[4])?,
                        options_json: string(row[5])?,
                        security: super::super::role_security::decode_relation_cells(
                            row[6], row[7], row[8],
                        )?,
                    });
                    Ok(())
                },
            )?;
            let acls = snapshot.load_relation_acls()?;
            for table in &mut tables {
                acls.apply(&table.relation, &mut table.security)?;
            }
            Ok(tables)
        })
    }
}
