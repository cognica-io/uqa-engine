//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign definitions and security share the native catalog transaction.

use super::{
    optional_text, string, text, Catalog, Family, NativeRecordOwner, RelationRecord, Result,
};
use crate::catalog::{ForeignTableRow, RelationIdentity};

type ServerRow = (String, String, String);

impl Catalog {
    pub(in crate::catalog) fn load_native_foreign_servers(&self) -> Result<Option<Vec<ServerRow>>> {
        self.read_native(|snapshot| {
            let mut servers = Vec::new();
            snapshot.visit_rows(
                Family::ForeignServers,
                Some(NativeRecordOwner::Database(snapshot.database)),
                &[],
                |row| {
                    servers.push((string(row[0])?, string(row[1])?, string(row[2])?));
                    Ok(())
                },
            )?;
            Ok(servers)
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
            Ok(tables)
        })
    }
}
