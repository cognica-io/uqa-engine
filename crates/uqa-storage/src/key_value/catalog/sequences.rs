//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic durable sequence state and allocation.

use super::{
    decode_relation_key, decode_value, encode_value, key_with_tag, relation_key, KeyValueCatalog,
    RelationIdentity, RelationKind, SequenceOptions, SequenceReservationResult, SequenceRow,
    StorageBackendError, StorageBackendResult, StoredRelation, StoredSequence, TAG_RELATION,
    TAG_SEQUENCE,
};
use crate::catalog::sequence_value_reservation;

fn concrete_sequence_options(sequence: &SequenceRow) -> SequenceOptions {
    let default_min = if sequence.increment > 0 { 1 } else { i64::MIN };
    let default_max = if sequence.increment > 0 { i64::MAX } else { -1 };
    SequenceOptions {
        data_type: sequence.options.data_type.clone(),
        min_value: Some(sequence.options.min_value.unwrap_or(default_min)),
        max_value: Some(sequence.options.max_value.unwrap_or(default_max)),
        cycle: sequence.options.cycle,
        cache_size: sequence.options.cache_size,
    }
}

fn sequence_bounds(stored: &StoredSequence) -> (i64, i64) {
    let default_min = if stored.increment > 0 { 1 } else { i64::MIN };
    let default_max = if stored.increment > 0 { i64::MAX } else { -1 };
    (
        stored.options.min_value.unwrap_or(default_min),
        stored.options.max_value.unwrap_or(default_max),
    )
}

fn validate_sequence_persistence(code: &str) -> StorageBackendResult<()> {
    if matches!(code, "p" | "u") {
        Ok(())
    } else {
        Err(StorageBackendError::Other(format!(
            "invalid durable sequence persistence `{code}`"
        )))
    }
}

impl KeyValueCatalog {
    pub(super) fn create_sequence_row_impl(
        &self,
        sequence: &SequenceRow,
    ) -> StorageBackendResult<bool> {
        let _guard = self.sequence_lock.lock();
        validate_sequence_persistence(&sequence.persistence)?;
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
                object_id: sequence.object_id,
                definition_generation: sequence.definition_generation,
                start: sequence.start,
                increment: sequence.increment,
                current: sequence.current,
                called: sequence.called,
                persistence: sequence.persistence.clone(),
                owner: sequence.owner,
                options: concrete_sequence_options(sequence),
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
        validate_sequence_persistence(&sequence.persistence)?;
        let key = relation_key(TAG_SEQUENCE, &sequence.relation)?;
        if self.store.get(&key)?.is_none() {
            return Ok(false);
        }
        self.store.put(
            &key,
            &encode_value(&StoredSequence {
                object_id: sequence.object_id,
                definition_generation: sequence.definition_generation,
                start: sequence.start,
                increment: sequence.increment,
                current: sequence.current,
                called: sequence.called,
                persistence: sequence.persistence.clone(),
                owner: sequence.owner,
                options: concrete_sequence_options(sequence),
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
                    object_id: stored.object_id,
                    definition_generation: stored.definition_generation,
                    start: stored.start,
                    increment: stored.increment,
                    current: stored.current,
                    called: stored.called,
                    persistence: stored.persistence,
                    owner: stored.owner,
                    options: stored.options,
                })
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        rows.sort_by(|left, right| left.relation.cmp(&right.relation));
        Ok(rows)
    }

    pub(super) fn reserve_sequence_values_impl(
        &self,
        name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> StorageBackendResult<SequenceReservationResult> {
        let _guard = self.sequence_lock.lock();
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let key = relation_key(TAG_SEQUENCE, &relation)?;
        let Some(value) = self.store.get(&key)? else {
            return Ok(SequenceReservationResult::Missing);
        };
        let mut stored: StoredSequence = decode_value(&value)?;
        if stored.object_id != object_id {
            return Ok(SequenceReservationResult::Missing);
        }
        if stored.definition_generation != definition_generation {
            return Ok(SequenceReservationResult::DefinitionChanged);
        }
        let (min_value, max_value) = sequence_bounds(&stored);
        if stored.increment == 0 || stored.options.cache_size <= 0 {
            return Err(StorageBackendError::Other(format!(
                "corrupt sequence `{name}` has increment {} and cache size {}",
                stored.increment, stored.options.cache_size
            )));
        }
        let Some(reservation) = sequence_value_reservation(
            stored.current,
            stored.called,
            stored.increment,
            min_value,
            max_value,
            stored.options.cycle,
            stored.options.cache_size,
        ) else {
            return Ok(SequenceReservationResult::Exhausted);
        };
        stored.current = reservation.last_value;
        stored.called = true;
        self.store.put(&key, &encode_value(&stored)?)?;
        Ok(SequenceReservationResult::Reserved(reservation))
    }

    pub(super) fn set_sequence_value_impl(
        &self,
        name: &str,
        object_id: [u8; 16],
        value: i64,
        called: bool,
    ) -> StorageBackendResult<Option<i64>> {
        let _guard = self.sequence_lock.lock();
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let key = relation_key(TAG_SEQUENCE, &relation)?;
        let Some(encoded) = self.store.get(&key)? else {
            return Ok(None);
        };
        let mut stored: StoredSequence = decode_value(&encoded)?;
        if stored.object_id != object_id {
            return Ok(None);
        }
        stored.current = value;
        stored.called = called;
        self.store.put(&key, &encode_value(&stored)?)?;
        Ok(Some(value))
    }
}
