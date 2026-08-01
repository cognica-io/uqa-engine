//! Typed view ownership and definitions.

use super::{
    decode_relation_key, decode_value, encode_value, key_with_tag, relation_key, KeyValueCatalog,
    RelationIdentity, RelationKind, StorageBackendResult, StoredView, ViewRow, TAG_VIEW,
};

impl KeyValueCatalog {
    pub(super) fn save_view_impl(&self, view: &ViewRow) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), &view.relation, RelationKind::View)?;
        batch.put(
            &relation_key(TAG_VIEW, &view.relation)?,
            &encode_value(&StoredView {
                definition_json: view.definition_json.clone(),
            })?,
        )?;
        batch.commit()
    }

    pub(super) fn drop_view_impl(&self, relation: &RelationIdentity) -> StorageBackendResult<bool> {
        let key = relation_key(TAG_VIEW, relation)?;
        if self.store.get(&key)?.is_none() {
            return Ok(false);
        }
        let mut batch = self.store.batch();
        batch.delete(&key)?;
        self.release_relation(batch.as_mut(), relation, RelationKind::View)?;
        batch.commit()?;
        Ok(true)
    }

    pub(super) fn load_views_impl(&self) -> StorageBackendResult<Vec<ViewRow>> {
        let mut rows = self
            .store
            .scan_prefix(&key_with_tag(TAG_VIEW))?
            .into_iter()
            .map(|(key, value)| {
                let relation = decode_relation_key(&key)?;
                let stored = decode_value::<StoredView>(&value)?;
                Ok(ViewRow {
                    relation,
                    definition_json: stored.definition_json,
                })
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        rows.sort_by(|left, right| left.relation.cmp(&right.relation));
        Ok(rows)
    }
}
