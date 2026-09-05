//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed view ownership and definitions.

use super::{
    decode_relation_key, decode_value, encode_value, key_with_tag, relation_key, KeyValueCatalog,
    RelationIdentity, RelationKind, StorageBackendError, StorageBackendResult, StoredView, ViewRow,
    TAG_RELATION, TAG_VIEW,
};

impl KeyValueCatalog {
    pub(super) fn save_view_impl(&self, view: &ViewRow) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), &view.relation, RelationKind::View)?;
        batch.put(
            &relation_key(TAG_VIEW, &view.relation)?,
            &encode_value(&StoredView {
                role_owner: view.role_owner.clone(),
                acl: view.acl.clone(),
                column_acls: view.column_acls.clone(),
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

    pub(super) fn rename_view_impl(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<bool> {
        let from_key = relation_key(TAG_VIEW, from)?;
        if from == to {
            return Ok(self.store.get(&from_key)?.is_some());
        }
        if from.schema != to.schema {
            return Err(StorageBackendError::Other(
                "moving a view between schemas is not supported by the catalog".into(),
            ));
        }
        let Some(value) = self.store.get(&from_key)? else {
            return Ok(false);
        };
        let to_key = relation_key(TAG_VIEW, to)?;
        if self.store.get(&to_key)?.is_some()
            || self.store.get(&relation_key(TAG_RELATION, to)?)?.is_some()
        {
            return Err(StorageBackendError::Other(format!(
                "relation `{}` already exists",
                to.qualified_name()
            )));
        }
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), to, RelationKind::View)?;
        batch.put(&to_key, &value)?;
        batch.delete(&from_key)?;
        self.release_relation(batch.as_mut(), from, RelationKind::View)?;
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
                    role_owner: stored.role_owner,
                    acl: stored.acl,
                    column_acls: stored.column_acls,
                    definition_json: stored.definition_json,
                })
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        rows.sort_by(|left, right| left.relation.cmp(&right.relation));
        Ok(rows)
    }
}
