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
    TableAclEntry, STORED_FOREIGN_TABLE_SECURITY_VERSION, TAG_FOREIGN_SERVER, TAG_FOREIGN_TABLE,
    TAG_RELATION,
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
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), &row.relation, RelationKind::ForeignTable)?;
        batch.put(
            &relation_key(TAG_FOREIGN_TABLE, &row.relation)?,
            &encode_value(&StoredForeignTable {
                security_version: STORED_FOREIGN_TABLE_SECURITY_VERSION,
                role_owner: row.role_owner.clone(),
                acl: row.acl.clone(),
                column_acls: row.column_acls.clone(),
                server_name: row.server_name.clone(),
                columns_json: row.columns_json.clone(),
                options_json: row.options_json.clone(),
            })?,
        )?;
        batch.commit()
    }

    pub(super) fn update_foreign_table_security_impl(
        &self,
        relation: &RelationIdentity,
        role_owner: &str,
        acl: Option<&[TableAclEntry]>,
        column_acls: &std::collections::BTreeMap<String, Vec<TableAclEntry>>,
    ) -> StorageBackendResult<bool> {
        let key = relation_key(TAG_FOREIGN_TABLE, relation)?;
        let Some(value) = self.store.get(&key)? else {
            return Ok(false);
        };
        let mut stored: StoredForeignTable = decode_value(&value)?;
        if stored.security_version != STORED_FOREIGN_TABLE_SECURITY_VERSION {
            return Err(StorageBackendError::Other(format!(
                "foreign-table catalog record `{}` has unsupported security version {}",
                relation.qualified_name(),
                stored.security_version
            )));
        }
        stored.role_owner = role_owner.to_string();
        stored.acl = acl.map(<[TableAclEntry]>::to_vec);
        stored.column_acls.clone_from(column_acls);
        self.store.put(&key, &encode_value(&stored)?)?;
        Ok(true)
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
        let Some(value) = self.store.get(&from_key)? else {
            return Ok(false);
        };
        let to_key = relation_key(TAG_FOREIGN_TABLE, to)?;
        if self.store.get(&to_key)?.is_some()
            || self.store.get(&relation_key(TAG_RELATION, to)?)?.is_some()
        {
            return Err(StorageBackendError::Other(format!(
                "relation `{}` already exists",
                to.qualified_name()
            )));
        }
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), to, RelationKind::ForeignTable)?;
        batch.put(&to_key, &value)?;
        batch.delete(&from_key)?;
        self.release_relation(batch.as_mut(), from, RelationKind::ForeignTable)?;
        batch.commit()?;
        Ok(true)
    }

    pub(super) fn drop_foreign_table_impl(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete(&relation_key(TAG_FOREIGN_TABLE, relation)?)?;
        self.release_relation(batch.as_mut(), relation, RelationKind::ForeignTable)?;
        batch.commit()
    }

    pub(super) fn load_foreign_tables_impl(&self) -> StorageBackendResult<Vec<ForeignTableRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_FOREIGN_TABLE))? {
            let relation = decode_relation_key(&key)?;
            let stored: StoredForeignTable = decode_value(&value)?;
            if stored.security_version != STORED_FOREIGN_TABLE_SECURITY_VERSION {
                return Err(StorageBackendError::Other(format!(
                    "foreign-table catalog record `{}` has unsupported security version {}",
                    relation.qualified_name(),
                    stored.security_version
                )));
            }
            rows.push(ForeignTableRow {
                relation,
                role_owner: stored.role_owner,
                acl: stored.acl,
                column_acls: stored.column_acls,
                server_name: stored.server_name,
                columns_json: stored.columns_json,
                options_json: stored.options_json,
            });
        }
        rows.sort_by(|a, b| a.relation.cmp(&b.relation));
        Ok(rows)
    }
}
