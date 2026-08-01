//! Atomic durable sequence state and allocation.

use super::{
    decode_relation_key, decode_value, encode_value, key_with_tag, relation_key, KeyValueCatalog,
    RelationIdentity, RelationKind, SequenceRow, StorageBackendError, StorageBackendResult,
    StoredRelation, StoredSequence, TAG_RELATION, TAG_SEQUENCE,
};

impl KeyValueCatalog {
    pub(super) fn create_sequence_row_impl(
        &self,
        sequence: &SequenceRow,
    ) -> StorageBackendResult<bool> {
        let _guard = self.sequence_lock.lock();
        self.ensure_schema_exists(&sequence.relation)?;
        let key = relation_key(TAG_SEQUENCE, &sequence.relation)?;
        if self.store.get(&key)?.is_some() {
            return Ok(false);
        }
        let relation_key = relation_key(TAG_RELATION, &sequence.relation)?;
        if let Some(value) = self.store.get(&relation_key)? {
            let existing = decode_value::<StoredRelation>(&value)?.kind;
            if existing != RelationKind::Sequence {
                return Err(StorageBackendError::Other(format!(
                    "relation `{}` already exists as {}",
                    sequence.relation.qualified_name(),
                    existing.as_str()
                )));
            }
        }
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), &sequence.relation, RelationKind::Sequence)?;
        batch.put(
            &key,
            &encode_value(&StoredSequence {
                start: sequence.start,
                increment: sequence.increment,
                current: sequence.current,
                called: sequence.called,
            })?,
        )?;
        batch.commit()?;
        Ok(true)
    }

    pub(super) fn replace_sequence_row_impl(
        &self,
        sequence: &SequenceRow,
    ) -> StorageBackendResult<bool> {
        let _guard = self.sequence_lock.lock();
        let key = relation_key(TAG_SEQUENCE, &sequence.relation)?;
        if self.store.get(&key)?.is_none() {
            return Ok(false);
        }
        self.store.put(
            &key,
            &encode_value(&StoredSequence {
                start: sequence.start,
                increment: sequence.increment,
                current: sequence.current,
                called: sequence.called,
            })?,
        )?;
        Ok(true)
    }

    pub(super) fn drop_sequence_row_impl(&self, name: &str) -> StorageBackendResult<bool> {
        let _guard = self.sequence_lock.lock();
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let key = relation_key(TAG_SEQUENCE, &relation)?;
        let existed = self.store.get(&key)?.is_some();
        if existed {
            let mut batch = self.store.batch();
            batch.delete(&key)?;
            self.release_relation(batch.as_mut(), &relation, RelationKind::Sequence)?;
            batch.commit()?;
        }
        Ok(existed)
    }

    pub(super) fn load_sequence_rows_impl(&self) -> StorageBackendResult<Vec<SequenceRow>> {
        let mut rows = self
            .store
            .scan_prefix(&key_with_tag(TAG_SEQUENCE))?
            .into_iter()
            .map(|(key, value)| {
                let relation = decode_relation_key(&key)?;
                let stored: StoredSequence = decode_value(&value)?;
                Ok(SequenceRow {
                    relation,
                    start: stored.start,
                    increment: stored.increment,
                    current: stored.current,
                    called: stored.called,
                })
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        rows.sort_by(|left, right| left.relation.cmp(&right.relation));
        Ok(rows)
    }

    pub(super) fn next_sequence_value_impl(&self, name: &str) -> StorageBackendResult<Option<i64>> {
        let _guard = self.sequence_lock.lock();
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let key = relation_key(TAG_SEQUENCE, &relation)?;
        let Some(value) = self.store.get(&key)? else {
            return Ok(None);
        };
        let mut stored: StoredSequence = decode_value(&value)?;
        if stored.called {
            stored.current = stored
                .current
                .checked_add(stored.increment)
                .ok_or_else(|| {
                    crate::StorageBackendError::Other(format!("sequence `{name}` overflow"))
                })?;
        } else {
            stored.called = true;
        }
        let current = stored.current;
        self.store.put(&key, &encode_value(&stored)?)?;
        Ok(Some(current))
    }

    pub(super) fn set_sequence_value_impl(
        &self,
        name: &str,
        value: i64,
    ) -> StorageBackendResult<Option<i64>> {
        let _guard = self.sequence_lock.lock();
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let key = relation_key(TAG_SEQUENCE, &relation)?;
        let Some(encoded) = self.store.get(&key)? else {
            return Ok(None);
        };
        let mut stored: StoredSequence = decode_value(&encoded)?;
        stored.current = value;
        stored.called = true;
        self.store.put(&key, &encode_value(&stored)?)?;
        Ok(Some(value))
    }
}
