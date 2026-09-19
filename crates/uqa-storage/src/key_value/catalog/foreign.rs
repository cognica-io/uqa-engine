//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign server and foreign table persistence.

use super::{
    decode_relation_key, decode_value, encode_value, key_with_tag, read_str, relation_key,
    single_str_key, ForeignTableRow, KeyValueCatalog, RelationIdentity, RelationKind,
    StorageBackendError, StorageBackendResult, StoredForeignServer, StoredForeignTable,
    STORED_FOREIGN_TABLE_SECURITY_VERSION, TAG_FOREIGN_SERVER, TAG_FOREIGN_TABLE, TAG_RELATION,
};

impl KeyValueCatalog {
    pub(super) fn save_foreign_server_impl(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_FOREIGN_SERVER, name)?,
            &encode_value(&StoredForeignServer {
                fdw_type: fdw_type.to_string(),
                options_json: options_json.to_string(),
            })?,
        )
    }

    pub(super) fn drop_foreign_server_impl(&self, name: &str) -> StorageBackendResult<()> {
        self.store
            .delete(&single_str_key(TAG_FOREIGN_SERVER, name)?)
    }

    pub(super) fn load_foreign_servers_impl(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_FOREIGN_SERVER))? {
            let mut offset = 1;
            let name = read_str(&key, &mut offset)?;
            let stored: StoredForeignServer = decode_value(&value)?;
            rows.push((name, stored.fdw_type, stored.options_json));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows)
    }

    pub(super) fn save_foreign_table_impl(
        &self,
        row: &ForeignTableRow,
    ) -> StorageBackendResult<()> {
        self.store.with_mutation(&mut |read, batch| {
            super::relations::claim_relation(
                read,
                batch,
                &row.relation,
                RelationKind::ForeignTable,
            )?;
            super::relation_acl::clear(read, batch, &row.relation)?;
            batch.put(
                &relation_key(TAG_FOREIGN_TABLE, &row.relation)?,
                &encode_value(&StoredForeignTable {
                    security_version: STORED_FOREIGN_TABLE_SECURITY_VERSION,
                    security: row.security.clone(),
                    server_name: row.server_name.clone(),
                    columns_json: row.columns_json.clone(),
                    options_json: row.options_json.clone(),
                })?,
            )
        })
    }

    pub(super) fn update_foreign_table_security_impl(
        &self,
        relation: &RelationIdentity,
        security: &crate::RelationSecurityRow,
    ) -> StorageBackendResult<bool> {
        let key = relation_key(TAG_FOREIGN_TABLE, relation)?;
        let mut found = false;
        self.store.with_mutation(&mut |read, batch| {
            let Some(value) = read.get(&key)? else {
                return Ok(());
            };
            found = true;
            let mut stored: StoredForeignTable = decode_value(&value)?;
            if stored.security_version != STORED_FOREIGN_TABLE_SECURITY_VERSION {
                return Err(StorageBackendError::Other(format!(
                    "foreign-table catalog record `{}` has unsupported security version {}",
                    relation.qualified_name(),
                    stored.security_version
                )));
            }
            stored.security = security.clone();
            super::relation_acl::clear(read, batch, relation)?;
            batch.put(&key, &encode_value(&stored)?)
        })?;
        Ok(found)
    }

    pub(super) fn rename_foreign_table_impl(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<bool> {
        let from_key = relation_key(TAG_FOREIGN_TABLE, from)?;
        if from == to {
            return Ok(self.store.get(&from_key)?.is_some());
        }
        if from.schema != to.schema {
            return Err(StorageBackendError::Other(
                "moving a foreign table between schemas is not supported by the catalog".into(),
            ));
        }
        let mut found = false;
        self.store.with_mutation(&mut |read, batch| {
            let Some(value) = read.get(&from_key)? else {
                return Ok(());
            };
            found = true;
            let to_key = relation_key(TAG_FOREIGN_TABLE, to)?;
            if read.get(&to_key)?.is_some() || read.get(&relation_key(TAG_RELATION, to)?)?.is_some()
            {
                return Err(StorageBackendError::Other(format!(
                    "relation `{}` already exists",
                    to.qualified_name()
                )));
            }
            super::relations::claim_relation(read, batch, to, RelationKind::ForeignTable)?;
            super::relation_acl::rename(read, batch, from, to)?;
            batch.put(&to_key, &value)?;
            batch.delete(&from_key)?;
            super::relations::release_relation(read, batch, from, RelationKind::ForeignTable)
        })?;
        Ok(found)
    }

    pub(super) fn drop_foreign_table_impl(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        self.store.with_mutation(&mut |read, batch| {
            super::relation_acl::clear(read, batch, relation)?;
            batch.delete(&relation_key(TAG_FOREIGN_TABLE, relation)?)?;
            super::relations::release_relation(read, batch, relation, RelationKind::ForeignTable)
        })
    }

    pub(super) fn load_foreign_tables_impl(&self) -> StorageBackendResult<Vec<ForeignTableRow>> {
        crate::key_value::index_view::read_view(self.store.as_ref(), |read| {
            let mut rows = Vec::new();
            read.visit_prefix(&[TAG_FOREIGN_TABLE], &mut |key, value| {
                let relation = decode_relation_key(key)?;
                let stored: StoredForeignTable = decode_value(value)?;
                if stored.security_version != STORED_FOREIGN_TABLE_SECURITY_VERSION {
                    return Err(StorageBackendError::Other(format!(
                        "foreign-table catalog record `{}` has unsupported security version {}",
                        relation.qualified_name(),
                        stored.security_version
                    )));
                }
                rows.push(ForeignTableRow {
                    relation,
                    security: stored.security,
                    server_name: stored.server_name,
                    columns_json: stored.columns_json,
                    options_json: stored.options_json,
                });
                Ok(())
            })?;
            let acls = super::relation_acl::load(read)?;
            for row in &mut rows {
                acls.apply(&row.relation, &mut row.security)?;
            }
            rows.sort_by(|left, right| left.relation.cmp(&right.relation));
            Ok(rows)
        })
    }
}
