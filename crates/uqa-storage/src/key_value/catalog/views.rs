//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed view ownership and definitions.

use super::{
    decode_relation_key, decode_value, encode_value, relation_key, KeyValueCatalog,
    RelationIdentity, RelationKind, StorageBackendError, StorageBackendResult, StoredView, ViewRow,
    TAG_RELATION, TAG_VIEW,
};

impl KeyValueCatalog {
    pub(super) fn save_view_impl(&self, view: &ViewRow) -> StorageBackendResult<()> {
        self.store.with_mutation(&mut |read, batch| {
            super::relations::claim_relation(read, batch, &view.relation, RelationKind::View)?;
            super::relation_acl::clear(read, batch, &view.relation)?;
            batch.put(
                &relation_key(TAG_VIEW, &view.relation)?,
                &encode_value(&StoredView {
                    security: view.security.clone(),
                    definition_json: view.definition_json.clone(),
                })?,
            )
        })
    }

    pub(super) fn drop_view_impl(&self, relation: &RelationIdentity) -> StorageBackendResult<bool> {
        let mut found = false;
        self.store.with_mutation(&mut |read, batch| {
            let key = relation_key(TAG_VIEW, relation)?;
            found = read.get(&key)?.is_some();
            if !found {
                return Ok(());
            }
            super::relation_acl::clear(read, batch, relation)?;
            batch.delete(&key)?;
            super::relations::release_relation(read, batch, relation, RelationKind::View)
        })?;
        Ok(found)
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
        let mut found = false;
        self.store.with_mutation(&mut |read, batch| {
            let Some(value) = read.get(&from_key)? else {
                return Ok(());
            };
            found = true;
            let to_key = relation_key(TAG_VIEW, to)?;
            if read.get(&to_key)?.is_some() || read.get(&relation_key(TAG_RELATION, to)?)?.is_some()
            {
                return Err(StorageBackendError::Other(format!(
                    "relation `{}` already exists",
                    to.qualified_name()
                )));
            }
            super::relations::claim_relation(read, batch, to, RelationKind::View)?;
            super::relation_acl::rename(read, batch, from, to)?;
            batch.put(&to_key, &value)?;
            batch.delete(&from_key)?;
            super::relations::release_relation(read, batch, from, RelationKind::View)
        })?;
        Ok(found)
    }

    pub(super) fn load_views_impl(&self) -> StorageBackendResult<Vec<ViewRow>> {
        crate::key_value::index_view::read_view(self.store.as_ref(), |read| {
            let mut rows = Vec::new();
            read.visit_prefix(&[TAG_VIEW], &mut |key, value| {
                let relation = decode_relation_key(key)?;
                let stored: StoredView = decode_value(value)?;
                rows.push(ViewRow {
                    relation,
                    security: stored.security,
                    definition_json: stored.definition_json,
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
