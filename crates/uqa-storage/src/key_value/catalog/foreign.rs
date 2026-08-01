//! Foreign server and foreign table persistence.

use super::{
    decode_relation_key, decode_value, encode_value, key_with_tag, read_str, relation_key,
    single_str_key, ForeignTableRow, KeyValueCatalog, RelationIdentity, RelationKind,
    StorageBackendResult, StoredForeignServer, StoredForeignTable, TAG_FOREIGN_SERVER,
    TAG_FOREIGN_TABLE,
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
        relation: &RelationIdentity,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), relation, RelationKind::ForeignTable)?;
        batch.put(
            &relation_key(TAG_FOREIGN_TABLE, relation)?,
            &encode_value(&StoredForeignTable {
                server_name: server_name.to_string(),
                columns_json: columns_json.to_string(),
                options_json: options_json.to_string(),
            })?,
        )?;
        batch.commit()
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
            rows.push(ForeignTableRow {
                relation,
                server_name: stored.server_name,
                columns_json: stored.columns_json,
                options_json: stored.options_json,
            });
        }
        rows.sort_by(|a, b| a.relation.cmp(&b.relation));
        Ok(rows)
    }
}
